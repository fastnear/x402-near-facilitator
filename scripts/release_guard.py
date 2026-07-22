#!/usr/bin/env python3
"""Fail-closed validation helpers for the tagged release workflow."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

TAG_RE = re.compile(r"^v([0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?)$")
DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


class GuardError(ValueError):
    """A release invariant was not satisfied."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise GuardError(message)


def read_json(path: Path) -> Any:
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def expected_image_labels(
    repository: str, version: str, commit: str
) -> dict[str, str]:
    return {
        "org.opencontainers.image.description": (
            "Rust x402 v2 exact Circle USDC facilitator for NEAR"
        ),
        "org.opencontainers.image.licenses": "Apache-2.0",
        "org.opencontainers.image.revision": commit,
        "org.opencontainers.image.source": f"https://github.com/{repository}",
        "org.opencontainers.image.title": (
            "FastNEAR x402 facilitator for NEAR"
        ),
        "org.opencontainers.image.version": version,
    }


def validate_image_labels(
    actual: dict[str, Any], repository: str, version: str, commit: str
) -> None:
    require(
        actual == expected_image_labels(repository, version, commit),
        "OCI image labels do not match the release source",
    )


def validate_source_documents(
    *,
    repository: dict[str, Any],
    tag_ref: dict[str, Any],
    tag_object: dict[str, Any],
    branch_ref: dict[str, Any],
    branch: dict[str, Any],
    repository_name: str,
    tag: str,
    workspace_version: str,
    expected_commit: str,
    github_sha: str,
    descendant_proven: bool = False,
) -> dict[str, str]:
    match = TAG_RE.fullmatch(tag)
    require(match is not None, f"release tag is not strict semver: {tag}")
    version = match.group(1)
    require(
        workspace_version == version,
        f"workspace version {workspace_version} does not match tag {tag}",
    )
    require(COMMIT_RE.fullmatch(expected_commit) is not None, "invalid checkout commit")
    require(COMMIT_RE.fullmatch(github_sha) is not None, "invalid GITHUB_SHA")

    require(
        str(repository.get("full_name", "")).lower() == repository_name.lower(),
        "repository API response does not match GITHUB_REPOSITORY",
    )
    default_branch = repository.get("default_branch")
    require(
        isinstance(default_branch, str) and default_branch,
        "repository default branch is missing",
    )
    require(branch.get("name") == default_branch, "default branch response mismatch")
    require(branch.get("protected") is True, "GitHub default branch is not protected")

    tag_ref_object = tag_ref.get("object") or {}
    require(tag_ref.get("ref") == f"refs/tags/{tag}", "tag ref name mismatch")
    require(
        tag_ref_object.get("type") == "tag",
        "release tag must be an annotated tag object",
    )
    tag_object_sha = tag_ref_object.get("sha")
    require(
        isinstance(tag_object_sha, str)
        and COMMIT_RE.fullmatch(tag_object_sha) is not None,
        "annotated tag object SHA is invalid",
    )

    require(tag_object.get("sha") == tag_object_sha, "tag object SHA mismatch")
    require(tag_object.get("tag") == tag, "annotated tag name mismatch")
    verification = tag_object.get("verification") or {}
    require(
        verification.get("verified") is True
        and verification.get("reason") == "valid",
        "annotated tag is not verified as valid by GitHub",
    )
    tagged_object = tag_object.get("object") or {}
    require(
        tagged_object.get("type") == "commit",
        "annotated tag must point directly to a commit",
    )
    tag_commit = tagged_object.get("sha")
    require(
        isinstance(tag_commit, str) and COMMIT_RE.fullmatch(tag_commit) is not None,
        "tagged commit SHA is invalid",
    )

    branch_object = branch_ref.get("object") or {}
    require(
        branch_ref.get("ref") == f"refs/heads/{default_branch}",
        "default branch ref name mismatch",
    )
    require(
        branch_object.get("type") == "commit",
        "default branch ref does not point to a commit",
    )
    branch_head = branch_object.get("sha")
    require(
        isinstance(branch_head, str) and COMMIT_RE.fullmatch(branch_head) is not None,
        "default branch head SHA is invalid",
    )
    if branch_head != tag_commit:
        require(
            descendant_proven,
            "tagged commit is not the current GitHub default branch head",
        )
    require(expected_commit == tag_commit, "checkout HEAD does not match tagged commit")
    require(github_sha == tag_commit, "GITHUB_SHA does not match tagged commit")

    stable = "true" if "-" not in version else "false"
    return {
        "commit": tag_commit,
        "default_branch_head": branch_head,
        "default_branch": default_branch,
        "image": f"ghcr.io/{repository_name.lower()}",
        "stable": stable,
        "tag_object": tag_object_sha,
        "version": version,
    }


def validate_checkpoint_document(
    *,
    release: dict[str, Any],
    tag: str,
    commit: str,
    body: str,
    stable: bool,
    allow_published: bool = False,
) -> tuple[int, bool]:
    release_id = release.get("id")
    require(
        isinstance(release_id, int) and release_id > 0,
        "draft release ID is invalid",
    )
    author = release.get("author") or {}
    require(
        isinstance(author, dict)
        and author.get("login") == "github-actions[bot]"
        and author.get("type") == "Bot",
        "release checkpoint author is not github-actions[bot]",
    )
    require(release.get("tag_name") == tag, "draft release tag mismatch")
    require(
        release.get("target_commitish") == commit,
        "draft release target commit mismatch",
    )
    require(release.get("name") == tag, "draft release title mismatch")
    require(release.get("body") == body, "draft release checkpoint body mismatch")
    require(
        release.get("prerelease") is (not stable),
        "draft release prerelease state mismatch",
    )
    draft = release.get("draft")
    require(isinstance(draft, bool), "release draft state is invalid")
    if draft:
        require(
            release.get("published_at") is None,
            "draft release unexpectedly has a publication timestamp",
        )
        require(
            release.get("immutable") in (None, False),
            "draft release is unexpectedly immutable",
        )
        return release_id, False

    require(allow_published, "release checkpoint is not a draft")
    require(
        isinstance(release.get("published_at"), str)
        and bool(release["published_at"]),
        "published release has no publication timestamp",
    )
    require(
        release.get("immutable") is True,
        "published release is not immutable",
    )
    return release_id, True


def select_release_document(
    release_pages: list[Any], tag: str
) -> dict[str, Any] | None:
    require(TAG_RE.fullmatch(tag) is not None, "release tag is invalid")
    matching: list[dict[str, Any]] = []
    for page in release_pages:
        require(isinstance(page, list), "paginated releases response is invalid")
        for release in page:
            require(isinstance(release, dict), "release response entry is invalid")
            if release.get("tag_name") == tag:
                matching.append(release)
    require(
        len(matching) <= 1,
        f"multiple GitHub releases use tag {tag}",
    )
    return matching[0] if matching else None


def decide_latest_alias_action(
    *,
    release_was_published: bool,
    release_id: int,
    tag: str,
    github_latest_release: dict[str, Any],
    current_digest: str | None,
    expected_digest: str,
) -> str:
    require(DIGEST_RE.fullmatch(expected_digest) is not None, "expected digest is invalid")
    require(
        current_digest is None or DIGEST_RE.fullmatch(current_digest) is not None,
        "current latest digest is invalid",
    )
    is_github_latest = (
        github_latest_release.get("id") == release_id
        and github_latest_release.get("tag_name") == tag
    )
    if is_github_latest:
        return "accept" if current_digest == expected_digest else "publish"
    require(
        release_was_published,
        "newly published stable release is not GitHub's latest release",
    )
    return "preserve"


def stable_version(tag: str) -> tuple[int, int, int] | None:
    match = TAG_RE.fullmatch(tag)
    if match is None or "-" in match.group(1):
        return None
    major, minor, patch = match.group(1).split(".")
    return int(major), int(minor), int(patch)


def decide_stable_publication(
    *,
    release_pages: list[Any],
    tag: str,
    release_was_published: bool,
) -> str:
    current = stable_version(tag)
    require(current is not None, "current release tag is not stable semver")
    higher_tags: set[str] = set()
    for page in release_pages:
        require(isinstance(page, list), "paginated releases response is invalid")
        for release in page:
            require(isinstance(release, dict), "release response entry is invalid")
            candidate_tag = release.get("tag_name")
            candidate = (
                stable_version(candidate_tag)
                if isinstance(candidate_tag, str)
                else None
            )
            if (
                candidate is not None
                and candidate > current
                and release.get("draft") is False
                and release.get("prerelease") is False
                and isinstance(release.get("published_at"), str)
                and bool(release["published_at"])
            ):
                higher_tags.add(candidate_tag)
    if higher_tags:
        require(
            release_was_published,
            "refusing to publish stale draft after higher stable release: "
            + ", ".join(sorted(higher_tags)),
        )
        return "preserve"
    return "promote"


def normalize_sbom_document(document: dict[str, Any], commit: str) -> dict[str, Any]:
    normalized = dict(document)
    normalized.pop("serialNumber", None)
    metadata = dict(normalized.get("metadata") or {})
    metadata.pop("timestamp", None)
    properties = [
        item
        for item in metadata.get("properties", [])
        if item.get("name") != "org.opencontainers.image.revision"
    ]
    properties.append(
        {
            "name": "org.opencontainers.image.revision",
            "value": commit,
        }
    )
    metadata["properties"] = sorted(
        properties,
        key=lambda item: (str(item.get("name", "")), str(item.get("value", ""))),
    )
    normalized["metadata"] = metadata
    return normalized


def normalize_sbom(source: Path, output: Path, commit: str) -> None:
    require(COMMIT_RE.fullmatch(commit) is not None, "invalid source commit")
    document = read_json(source)
    require(isinstance(document, dict), "SBOM root must be an object")
    normalized = normalize_sbom_document(document, commit)
    with output.open("w", encoding="utf-8") as destination:
        json.dump(normalized, destination, indent=2, sort_keys=True)
        destination.write("\n")


def build_manifest(
    *,
    dist: Path,
    output: Path,
    repository: str,
    tag: str,
    tag_object: str,
    commit: str,
    default_branch: str,
    version: str,
    image: str,
    image_digest: str,
) -> None:
    require(TAG_RE.fullmatch(tag) is not None, "manifest tag is invalid")
    require(COMMIT_RE.fullmatch(tag_object) is not None, "tag object SHA is invalid")
    require(COMMIT_RE.fullmatch(commit) is not None, "commit SHA is invalid")
    require(DIGEST_RE.fullmatch(image_digest) is not None, "image digest is invalid")
    require(version == tag[1:], "manifest version does not match tag")
    require(image == f"ghcr.io/{repository.lower()}", "manifest image mismatch")

    assets: list[dict[str, Any]] = []
    for path in sorted(dist.iterdir(), key=lambda candidate: candidate.name):
        if path == output:
            continue
        require(path.is_file() and not path.is_symlink(), f"invalid asset: {path.name}")
        assets.append(
            {
                "label": path.name,
                "name": path.name,
                "sha256": sha256(path),
                "size": path.stat().st_size,
            }
        )
    require(assets, "release manifest has no assets")

    manifest = {
        "assets": assets,
        "oci": {
            "digest": image_digest,
            "labels": expected_image_labels(repository, version, commit),
            "reference": f"{image}@{image_digest}",
        },
        "schema": 1,
        "source": {
            "commit": commit,
            "default_branch": default_branch,
            "repository": repository,
            "tag": tag,
            "tag_object": tag_object,
            "version": version,
        },
    }
    with output.open("w", encoding="utf-8") as destination:
        json.dump(manifest, destination, indent=2, sort_keys=True)
        destination.write("\n")


def validate_manifest_document(
    *,
    manifest: dict[str, Any],
    dist: Path,
    manifest_path: Path,
    repository: str,
    tag: str,
    tag_object: str,
    commit: str,
    default_branch: str,
    version: str,
    image: str,
    image_digest: str,
) -> set[str]:
    require(
        set(manifest) == {"assets", "oci", "schema", "source"},
        "release manifest fields are not exact",
    )
    require(
        manifest.get("schema") == 1
        and not isinstance(manifest.get("schema"), bool),
        "release manifest schema is invalid",
    )
    require(
        manifest.get("source")
        == {
            "commit": commit,
            "default_branch": default_branch,
            "repository": repository,
            "tag": tag,
            "tag_object": tag_object,
            "version": version,
        },
        "release manifest source does not match expected source",
    )
    require(
        manifest.get("oci")
        == {
            "digest": image_digest,
            "labels": expected_image_labels(repository, version, commit),
            "reference": f"{image}@{image_digest}",
        },
        "release manifest OCI identity does not match expected image",
    )

    listed_assets = manifest.get("assets")
    require(
        isinstance(listed_assets, list) and bool(listed_assets),
        "release manifest assets are invalid",
    )
    expected_names: set[str] = set()
    expected_labels: set[str] = set()
    for item in listed_assets:
        require(isinstance(item, dict), "release manifest asset is invalid")
        require(
            set(item) == {"label", "name", "sha256", "size"},
            "release manifest asset fields are not exact",
        )
        name = item.get("name")
        label = item.get("label")
        require(
            isinstance(name, str)
            and bool(name)
            and Path(name).name == name
            and name != manifest_path.name,
            "release manifest asset name is invalid",
        )
        require(name not in expected_names, f"duplicate manifest asset name: {name}")
        require(
            isinstance(label, str) and label == name,
            f"release manifest asset label mismatch: {name}",
        )
        require(
            label not in expected_labels,
            f"duplicate manifest asset label: {label}",
        )
        expected_names.add(name)
        expected_labels.add(label)

        local_path = dist / name
        require(
            local_path.is_file() and not local_path.is_symlink(),
            f"manifest asset is not a regular local file: {name}",
        )
        size = item.get("size")
        require(
            isinstance(size, int)
            and not isinstance(size, bool)
            and size == local_path.stat().st_size,
            f"manifest asset size mismatch: {name}",
        )
        digest = item.get("sha256")
        require(
            isinstance(digest, str)
            and SHA256_RE.fullmatch(digest) is not None
            and digest == sha256(local_path),
            f"manifest asset SHA-256 mismatch: {name}",
        )

    return expected_names


def validate_asset_documents(
    *,
    release: dict[str, Any],
    dist: Path,
    manifest_path: Path,
    allow_missing: bool,
    repository: str,
    tag: str,
    tag_object: str,
    commit: str,
    default_branch: str,
    version: str,
    image: str,
    image_digest: str,
) -> tuple[list[str], dict[str, int]]:
    manifest = read_json(manifest_path)
    require(isinstance(manifest, dict), "release manifest root is invalid")
    expected_names = validate_manifest_document(
        manifest=manifest,
        dist=dist,
        manifest_path=manifest_path,
        repository=repository,
        tag=tag,
        tag_object=tag_object,
        commit=commit,
        default_branch=default_branch,
        version=version,
        image=image,
        image_digest=image_digest,
    )
    expected_names.add(manifest_path.name)

    local_names = {
        path.name
        for path in dist.iterdir()
        if path.is_file() and not path.is_symlink()
    }
    require(local_names == expected_names, "local release asset set is not exact")

    remote_assets = release.get("assets")
    require(isinstance(remote_assets, list), "release assets response is invalid")
    by_name: dict[str, dict[str, Any]] = {}
    for asset in remote_assets:
        name = asset.get("name")
        require(isinstance(name, str) and name, "release asset has no name")
        require(name not in by_name, f"duplicate release asset name: {name}")
        by_name[name] = asset
    unexpected = sorted(set(by_name) - expected_names)
    require(not unexpected, f"unexpected release assets: {', '.join(unexpected)}")

    missing: list[str] = []
    asset_ids: dict[str, int] = {}
    for name in sorted(expected_names):
        local_path = dist / name
        remote = by_name.get(name)
        if remote is None:
            missing.append(name)
            continue
        require(remote.get("state") == "uploaded", f"asset is not uploaded: {name}")
        require(remote.get("label") == name, f"asset label mismatch: {name}")
        require(remote.get("size") == local_path.stat().st_size, f"asset size mismatch: {name}")
        remote_digest = remote.get("digest")
        if remote_digest:
            require(
                remote_digest == f"sha256:{sha256(local_path)}",
                f"asset digest mismatch: {name}",
            )
        asset_id = remote.get("id")
        require(isinstance(asset_id, int) and asset_id > 0, f"asset ID is invalid: {name}")
        asset_ids[name] = asset_id

    require(allow_missing or not missing, f"missing release assets: {', '.join(missing)}")
    return missing, asset_ids


def write_outputs(path: Path | None, values: dict[str, str]) -> None:
    if path is None:
        return
    with path.open("a", encoding="utf-8") as output:
        for key, value in values.items():
            require("\n" not in value, f"multiline output is forbidden: {key}")
            output.write(f"{key}={value}\n")


def command_validate_source(args: argparse.Namespace) -> None:
    values = validate_source_documents(
        repository=read_json(args.repository_json),
        tag_ref=read_json(args.tag_ref_json),
        tag_object=read_json(args.tag_object_json),
        branch_ref=read_json(args.branch_ref_json),
        branch=read_json(args.branch_json),
        repository_name=args.repository,
        tag=args.tag,
        workspace_version=args.workspace_version,
        expected_commit=args.expected_commit,
        github_sha=args.github_sha,
        descendant_proven=args.descendant_proven,
    )
    write_outputs(args.github_output, values)
    print(json.dumps(values, sort_keys=True))


def command_validate_checkpoint(args: argparse.Namespace) -> None:
    release_id, published = validate_checkpoint_document(
        release=read_json(args.release_json),
        tag=args.tag,
        commit=args.commit,
        body=args.body_file.read_text(encoding="utf-8"),
        stable=args.stable == "true",
        allow_published=args.allow_published,
    )
    write_outputs(
        args.github_output,
        {
            "release_id": str(release_id),
            "release_published": "true" if published else "false",
        },
    )
    print(json.dumps({"published": published, "release_id": release_id}))


def command_select_release(args: argparse.Namespace) -> None:
    release_pages = read_json(args.releases_json)
    require(isinstance(release_pages, list), "paginated releases root is invalid")
    release = select_release_document(release_pages, args.tag)
    with args.release_output.open("w", encoding="utf-8") as destination:
        json.dump(release, destination, indent=2, sort_keys=True)
        destination.write("\n")
    print(json.dumps({"found": release is not None}, sort_keys=True))


def command_decide_latest(args: argparse.Namespace) -> None:
    latest_release = read_json(args.github_latest_release_json)
    require(isinstance(latest_release, dict), "latest release response is invalid")
    action = decide_latest_alias_action(
        release_was_published=args.release_was_published,
        release_id=args.release_id,
        tag=args.tag,
        github_latest_release=latest_release,
        current_digest=args.current_digest,
        expected_digest=args.expected_digest,
    )
    print(json.dumps({"action": action}, sort_keys=True))


def command_decide_stable(args: argparse.Namespace) -> None:
    release_pages = read_json(args.releases_json)
    require(isinstance(release_pages, list), "paginated releases root is invalid")
    action = decide_stable_publication(
        release_pages=release_pages,
        tag=args.tag,
        release_was_published=args.release_was_published,
    )
    print(json.dumps({"action": action}, sort_keys=True))


def command_normalize_sbom(args: argparse.Namespace) -> None:
    normalize_sbom(args.source, args.output, args.commit)


def command_build_manifest(args: argparse.Namespace) -> None:
    build_manifest(
        dist=args.dist,
        output=args.output,
        repository=args.repository,
        tag=args.tag,
        tag_object=args.tag_object,
        commit=args.commit,
        default_branch=args.default_branch,
        version=args.version,
        image=args.image,
        image_digest=args.image_digest,
    )


def command_validate_assets(args: argparse.Namespace) -> None:
    missing, asset_ids = validate_asset_documents(
        release=read_json(args.release_json),
        dist=args.dist,
        manifest_path=args.manifest,
        allow_missing=args.allow_missing,
        repository=args.repository,
        tag=args.tag,
        tag_object=args.tag_object,
        commit=args.commit,
        default_branch=args.default_branch,
        version=args.version,
        image=args.image,
        image_digest=args.image_digest,
    )
    if args.missing_output is not None:
        args.missing_output.write_text(
            "".join(f"{name}\n" for name in missing),
            encoding="utf-8",
        )
    if args.asset_map_output is not None:
        with args.asset_map_output.open("w", encoding="utf-8") as destination:
            json.dump(asset_ids, destination, sort_keys=True)
            destination.write("\n")
    print(json.dumps({"asset_ids": asset_ids, "missing": missing}, sort_keys=True))


def command_validate_labels(args: argparse.Namespace) -> None:
    actual = read_json(args.labels_json)
    require(isinstance(actual, dict), "OCI image labels response is invalid")
    validate_image_labels(actual, args.repository, args.version, args.commit)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(required=True)

    source = commands.add_parser("validate-source")
    source.add_argument("--repository-json", type=Path, required=True)
    source.add_argument("--tag-ref-json", type=Path, required=True)
    source.add_argument("--tag-object-json", type=Path, required=True)
    source.add_argument("--branch-ref-json", type=Path, required=True)
    source.add_argument("--branch-json", type=Path, required=True)
    source.add_argument("--repository", required=True)
    source.add_argument("--tag", required=True)
    source.add_argument("--workspace-version", required=True)
    source.add_argument("--expected-commit", required=True)
    source.add_argument("--github-sha", required=True)
    source.add_argument("--descendant-proven", action="store_true")
    source.add_argument("--github-output", type=Path)
    source.set_defaults(handler=command_validate_source)

    checkpoint = commands.add_parser("validate-checkpoint")
    checkpoint.add_argument("--release-json", type=Path, required=True)
    checkpoint.add_argument("--tag", required=True)
    checkpoint.add_argument("--commit", required=True)
    checkpoint.add_argument("--body-file", type=Path, required=True)
    checkpoint.add_argument("--stable", choices=("true", "false"), required=True)
    checkpoint.add_argument("--allow-published", action="store_true")
    checkpoint.add_argument("--github-output", type=Path)
    checkpoint.set_defaults(handler=command_validate_checkpoint)

    release = commands.add_parser("select-release")
    release.add_argument("--releases-json", type=Path, required=True)
    release.add_argument("--tag", required=True)
    release.add_argument("--release-output", type=Path, required=True)
    release.set_defaults(handler=command_select_release)

    latest = commands.add_parser("decide-latest")
    latest.add_argument("--release-was-published", action="store_true")
    latest.add_argument("--release-id", type=int, required=True)
    latest.add_argument("--tag", required=True)
    latest.add_argument("--github-latest-release-json", type=Path, required=True)
    latest.add_argument("--current-digest")
    latest.add_argument("--expected-digest", required=True)
    latest.set_defaults(handler=command_decide_latest)

    stable = commands.add_parser("decide-stable")
    stable.add_argument("--releases-json", type=Path, required=True)
    stable.add_argument("--tag", required=True)
    stable.add_argument("--release-was-published", action="store_true")
    stable.set_defaults(handler=command_decide_stable)

    sbom = commands.add_parser("normalize-sbom")
    sbom.add_argument("--source", type=Path, required=True)
    sbom.add_argument("--output", type=Path, required=True)
    sbom.add_argument("--commit", required=True)
    sbom.set_defaults(handler=command_normalize_sbom)

    manifest = commands.add_parser("build-manifest")
    manifest.add_argument("--dist", type=Path, required=True)
    manifest.add_argument("--output", type=Path, required=True)
    manifest.add_argument("--repository", required=True)
    manifest.add_argument("--tag", required=True)
    manifest.add_argument("--tag-object", required=True)
    manifest.add_argument("--commit", required=True)
    manifest.add_argument("--default-branch", required=True)
    manifest.add_argument("--version", required=True)
    manifest.add_argument("--image", required=True)
    manifest.add_argument("--image-digest", required=True)
    manifest.set_defaults(handler=command_build_manifest)

    assets = commands.add_parser("validate-assets")
    assets.add_argument("--release-json", type=Path, required=True)
    assets.add_argument("--dist", type=Path, required=True)
    assets.add_argument("--manifest", type=Path, required=True)
    assets.add_argument("--repository", required=True)
    assets.add_argument("--tag", required=True)
    assets.add_argument("--tag-object", required=True)
    assets.add_argument("--commit", required=True)
    assets.add_argument("--default-branch", required=True)
    assets.add_argument("--version", required=True)
    assets.add_argument("--image", required=True)
    assets.add_argument("--image-digest", required=True)
    assets.add_argument("--allow-missing", action="store_true")
    assets.add_argument("--missing-output", type=Path)
    assets.add_argument("--asset-map-output", type=Path)
    assets.set_defaults(handler=command_validate_assets)

    labels = commands.add_parser("validate-labels")
    labels.add_argument("--labels-json", type=Path, required=True)
    labels.add_argument("--repository", required=True)
    labels.add_argument("--version", required=True)
    labels.add_argument("--commit", required=True)
    labels.set_defaults(handler=command_validate_labels)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        args.handler(args)
    except (GuardError, OSError, json.JSONDecodeError, KeyError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
