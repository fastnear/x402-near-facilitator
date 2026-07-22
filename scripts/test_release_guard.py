#!/usr/bin/env python3
"""Deterministic tests for release workflow validation helpers."""

from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

import release_guard

COMMIT = "1" * 40
TAG_OBJECT = "2" * 40
REPOSITORY = "fastnear/x402-near-facilitator"
REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
IMAGE = f"ghcr.io/{REPOSITORY}"
IMAGE_DIGEST = f"sha256:{'4' * 64}"


def source_documents() -> tuple[dict, dict, dict, dict, dict]:
    repository = {"default_branch": "main", "full_name": REPOSITORY}
    tag_ref = {
        "ref": "refs/tags/v1.2.3",
        "object": {"sha": TAG_OBJECT, "type": "tag"},
    }
    tag_object = {
        "object": {"sha": COMMIT, "type": "commit"},
        "sha": TAG_OBJECT,
        "tag": "v1.2.3",
        "verification": {"reason": "valid", "verified": True},
    }
    branch_ref = {
        "ref": "refs/heads/main",
        "object": {"sha": COMMIT, "type": "commit"},
    }
    branch = {"name": "main", "protected": True}
    return repository, tag_ref, tag_object, branch_ref, branch


class SourceValidationTests(unittest.TestCase):
    def validate(
        self,
        documents: tuple[dict, dict, dict, dict, dict],
        *,
        descendant_proven: bool = False,
    ) -> dict[str, str]:
        repository, tag_ref, tag_object, branch_ref, branch = documents
        return release_guard.validate_source_documents(
            repository=repository,
            tag_ref=tag_ref,
            tag_object=tag_object,
            branch_ref=branch_ref,
            branch=branch,
            repository_name=REPOSITORY,
            tag="v1.2.3",
            workspace_version="1.2.3",
            expected_commit=COMMIT,
            github_sha=COMMIT,
            descendant_proven=descendant_proven,
        )

    def test_accepts_verified_annotated_tag_at_default_head(self) -> None:
        values = self.validate(source_documents())
        self.assertEqual(values["commit"], COMMIT)
        self.assertEqual(values["stable"], "true")
        self.assertEqual(values["tag_object"], TAG_OBJECT)

    def test_rejects_lightweight_tag(self) -> None:
        documents = list(copy.deepcopy(source_documents()))
        documents[1]["object"]["type"] = "commit"
        with self.assertRaisesRegex(release_guard.GuardError, "annotated"):
            self.validate(tuple(documents))

    def test_rejects_unverified_tag(self) -> None:
        documents = list(copy.deepcopy(source_documents()))
        documents[2]["verification"]["verified"] = False
        with self.assertRaisesRegex(release_guard.GuardError, "not verified"):
            self.validate(tuple(documents))

    def test_rejects_tag_that_is_not_default_branch_head(self) -> None:
        documents = list(copy.deepcopy(source_documents()))
        documents[3]["object"]["sha"] = "3" * 40
        with self.assertRaisesRegex(release_guard.GuardError, "default branch head"):
            self.validate(tuple(documents))

    def test_exact_checkpoint_can_resume_at_descendant_head(self) -> None:
        release_guard.validate_checkpoint_document(
            release=CheckpointTests.release(),
            tag="v1.2.3",
            commit=COMMIT,
            body="checkpoint\n",
            stable=True,
        )
        documents = list(copy.deepcopy(source_documents()))
        documents[3]["object"]["sha"] = "3" * 40
        values = self.validate(tuple(documents), descendant_proven=True)
        self.assertEqual(values["commit"], COMMIT)
        self.assertEqual(values["default_branch_head"], "3" * 40)

    def test_rejects_divergent_head_without_ancestry_proof(self) -> None:
        documents = list(copy.deepcopy(source_documents()))
        documents[3]["object"]["sha"] = "3" * 40
        with self.assertRaisesRegex(release_guard.GuardError, "default branch head"):
            self.validate(tuple(documents), descendant_proven=False)

    def test_rejects_unprotected_default_branch(self) -> None:
        documents = list(copy.deepcopy(source_documents()))
        documents[4]["protected"] = False
        with self.assertRaisesRegex(release_guard.GuardError, "not protected"):
            self.validate(tuple(documents))


class CheckpointTests(unittest.TestCase):
    @staticmethod
    def release() -> dict:
        return {
            "author": {"login": "github-actions[bot]", "type": "Bot"},
            "body": "checkpoint\n",
            "draft": True,
            "id": 42,
            "immutable": False,
            "name": "v1.2.3",
            "prerelease": False,
            "published_at": None,
            "tag_name": "v1.2.3",
            "target_commitish": COMMIT,
        }

    def test_accepts_exact_draft_checkpoint(self) -> None:
        release_id, published = release_guard.validate_checkpoint_document(
            release=self.release(),
            tag="v1.2.3",
            commit=COMMIT,
            body="checkpoint\n",
            stable=True,
        )
        self.assertEqual(release_id, 42)
        self.assertFalse(published)

    def test_rejects_published_checkpoint(self) -> None:
        release = self.release()
        release["draft"] = False
        with self.assertRaisesRegex(release_guard.GuardError, "not a draft"):
            release_guard.validate_checkpoint_document(
                release=release,
                tag="v1.2.3",
                commit=COMMIT,
                body="checkpoint\n",
                stable=True,
            )

    def test_accepts_exact_immutable_published_checkpoint_only_for_resume(
        self,
    ) -> None:
        release = self.release()
        release["draft"] = False
        release["published_at"] = "2026-07-19T00:00:00Z"
        release["immutable"] = True
        release_id, published = release_guard.validate_checkpoint_document(
            release=release,
            tag="v1.2.3",
            commit=COMMIT,
            body="checkpoint\n",
            stable=True,
            allow_published=True,
        )
        self.assertEqual(release_id, 42)
        self.assertTrue(published)

    def test_rejects_non_bot_checkpoint_author(self) -> None:
        release = self.release()
        release["author"] = {"login": "maintainer", "type": "User"}
        with self.assertRaisesRegex(release_guard.GuardError, "author"):
            release_guard.validate_checkpoint_document(
                release=release,
                tag="v1.2.3",
                commit=COMMIT,
                body="checkpoint\n",
                stable=True,
            )

    def test_rejects_mutable_published_checkpoint(self) -> None:
        release = self.release()
        release["draft"] = False
        release["published_at"] = "2026-07-19T00:00:00Z"
        with self.assertRaisesRegex(release_guard.GuardError, "immutable"):
            release_guard.validate_checkpoint_document(
                release=release,
                tag="v1.2.3",
                commit=COMMIT,
                body="checkpoint\n",
                stable=True,
                allow_published=True,
            )


class ReleaseDiscoveryTests(unittest.TestCase):
    def test_selects_none_when_tag_is_absent(self) -> None:
        selected = release_guard.select_release_document(
            [
                [{"id": 1, "tag_name": "v1.2.2"}],
                [{"id": 2, "tag_name": "v1.2.4"}],
            ],
            "v1.2.3",
        )
        self.assertIsNone(selected)

    def test_selects_one_draft_across_paginated_results(self) -> None:
        draft = {"draft": True, "id": 2, "tag_name": "v1.2.3"}
        selected = release_guard.select_release_document(
            [
                [{"id": 1, "tag_name": "v1.2.2"}],
                [draft, {"id": 3, "tag_name": "v1.2.4"}],
            ],
            "v1.2.3",
        )
        self.assertIs(selected, draft)

    def test_rejects_duplicate_matching_releases(self) -> None:
        with self.assertRaisesRegex(release_guard.GuardError, "multiple"):
            release_guard.select_release_document(
                [
                    [{"draft": True, "id": 1, "tag_name": "v1.2.3"}],
                    [{"draft": False, "id": 2, "tag_name": "v1.2.3"}],
                ],
                "v1.2.3",
            )


class StablePublicationTests(unittest.TestCase):
    @staticmethod
    def published(tag: str, release_id: int) -> dict:
        return {
            "draft": False,
            "id": release_id,
            "prerelease": False,
            "published_at": "2026-07-19T00:00:00Z",
            "tag_name": tag,
        }

    def test_rejects_stale_draft_when_newer_stable_exists(self) -> None:
        with self.assertRaisesRegex(release_guard.GuardError, "stale draft"):
            release_guard.decide_stable_publication(
                release_pages=[[self.published("v1.3.0", 2)]],
                tag="v1.2.3",
                release_was_published=False,
            )

    def test_published_older_release_preserves_latest_pointers(self) -> None:
        action = release_guard.decide_stable_publication(
            release_pages=[[self.published("v2.0.0", 2)]],
            tag="v1.2.3",
            release_was_published=True,
        )
        self.assertEqual(action, "preserve")

    def test_highest_stable_is_eligible_for_latest_pointers(self) -> None:
        action = release_guard.decide_stable_publication(
            release_pages=[
                [
                    self.published("v1.2.2", 1),
                    self.published("v1.3.0-rc.1", 2),
                ]
            ],
            tag="v1.2.3",
            release_was_published=False,
        )
        self.assertEqual(action, "promote")

    def test_latest_alias_repair_is_allowed_for_github_latest(self) -> None:
        action = release_guard.decide_latest_alias_action(
            release_was_published=True,
            release_id=42,
            tag="v1.2.3",
            github_latest_release={"id": 42, "tag_name": "v1.2.3"},
            current_digest=f"sha256:{'5' * 64}",
            expected_digest=IMAGE_DIGEST,
        )
        self.assertEqual(action, "publish")

    def test_latest_alias_never_rolls_back_after_newer_release(self) -> None:
        action = release_guard.decide_latest_alias_action(
            release_was_published=True,
            release_id=42,
            tag="v1.2.3",
            github_latest_release={"id": 43, "tag_name": "v1.3.0"},
            current_digest=f"sha256:{'5' * 64}",
            expected_digest=IMAGE_DIGEST,
        )
        self.assertEqual(action, "preserve")


class ArtifactTests(unittest.TestCase):
    def asset_fixture(
        self, dist: Path, *, digest: str | None
    ) -> tuple[dict, Path]:
        asset = dist / "artifact.txt"
        asset.write_text("artifact\n", encoding="utf-8")
        manifest = dist / "release-manifest.json"
        release_guard.build_manifest(
            dist=dist,
            output=manifest,
            repository=REPOSITORY,
            tag="v1.2.3",
            tag_object=TAG_OBJECT,
            commit=COMMIT,
            default_branch="main",
            version="1.2.3",
            image=IMAGE,
            image_digest=IMAGE_DIGEST,
        )

        def remote(path: Path, asset_id: int, remote_digest: str | None) -> dict:
            document = {
                "id": asset_id,
                "label": path.name,
                "name": path.name,
                "size": path.stat().st_size,
                "state": "uploaded",
            }
            if remote_digest is not None:
                document["digest"] = remote_digest
            return document

        return {
            "assets": [
                remote(asset, 1, digest),
                remote(manifest, 2, None),
            ]
        }, manifest

    def validate_assets(
        self,
        *,
        release: dict,
        dist: Path,
        manifest: Path,
        allow_missing: bool = False,
    ) -> tuple[list[str], dict[str, int]]:
        return release_guard.validate_asset_documents(
            release=release,
            dist=dist,
            manifest_path=manifest,
            allow_missing=allow_missing,
            repository=REPOSITORY,
            tag="v1.2.3",
            tag_object=TAG_OBJECT,
            commit=COMMIT,
            default_branch="main",
            version="1.2.3",
            image=IMAGE,
            image_digest=IMAGE_DIGEST,
        )

    def test_sbom_normalization_pins_run_specific_fields(self) -> None:
        original = {
            "bomFormat": "CycloneDX",
            "metadata": {"timestamp": "now"},
            "serialNumber": "urn:uuid:random",
        }
        normalized = release_guard.normalize_sbom_document(original, COMMIT)
        self.assertRegex(
            normalized["serialNumber"],
            r"^urn:uuid:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}"
            r"-[0-9a-f]{4}-[0-9a-f]{12}$",
        )
        self.assertNotEqual(normalized["serialNumber"], "urn:uuid:random")
        self.assertEqual(
            normalized["serialNumber"],
            release_guard.normalize_sbom_document(original, COMMIT)[
                "serialNumber"
            ],
        )
        self.assertNotIn("timestamp", normalized["metadata"])
        self.assertEqual(
            normalized["metadata"]["properties"],
            [
                {
                    "name": "org.opencontainers.image.revision",
                    "value": COMMIT,
                }
            ],
        )

    def test_manifest_is_byte_deterministic_and_binds_assets(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            (dist / "b.txt").write_text("b\n", encoding="utf-8")
            (dist / "a.txt").write_text("a\n", encoding="utf-8")
            manifest = dist / "release-manifest.json"
            arguments = {
                "dist": dist,
                "output": manifest,
                "repository": REPOSITORY,
                "tag": "v1.2.3",
                "tag_object": TAG_OBJECT,
                "commit": COMMIT,
                "default_branch": "main",
                "version": "1.2.3",
                "image": IMAGE,
                "image_digest": IMAGE_DIGEST,
            }
            release_guard.build_manifest(**arguments)
            first = manifest.read_bytes()
            release_guard.build_manifest(**arguments)
            self.assertEqual(first, manifest.read_bytes())
            document = json.loads(first)
            self.assertEqual(
                [asset["name"] for asset in document["assets"]],
                ["a.txt", "b.txt"],
            )
            self.assertEqual(document["source"]["commit"], COMMIT)

    def test_resume_accepts_asset_when_api_digest_is_absent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            release, manifest = self.asset_fixture(dist, digest=None)
            missing, asset_ids = self.validate_assets(
                release=release,
                dist=dist,
                manifest=manifest,
                allow_missing=False,
            )
            self.assertEqual(missing, [])
            self.assertEqual(asset_ids, {"artifact.txt": 1, manifest.name: 2})

    def test_resume_rejects_mismatching_api_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            release, manifest = self.asset_fixture(
                dist, digest=f"sha256:{'0' * 64}"
            )
            with self.assertRaisesRegex(release_guard.GuardError, "digest mismatch"):
                self.validate_assets(
                    release=release,
                    dist=dist,
                    manifest=manifest,
                    allow_missing=False,
                )

    def test_resume_rejects_attestation_bundle_as_release_asset(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            release, manifest = self.asset_fixture(dist, digest=None)
            release["assets"].append(
                {
                    "id": 3,
                    "label": "provenance.sigstore.json",
                    "name": "provenance.sigstore.json",
                    "size": 1,
                    "state": "uploaded",
                }
            )
            with self.assertRaisesRegex(
                release_guard.GuardError, "unexpected release assets"
            ):
                self.validate_assets(
                    release=release,
                    dist=dist,
                    manifest=manifest,
                    allow_missing=False,
                )

    def test_rejects_manifest_identity_and_asset_drift(self) -> None:
        mutations = {
            "schema": (
                lambda document: document.__setitem__("schema", 2),
                "schema",
            ),
            "source": (
                lambda document: document["source"].__setitem__(
                    "commit", "3" * 40
                ),
                "source",
            ),
            "oci": (
                lambda document: document["oci"].__setitem__(
                    "digest", f"sha256:{'5' * 64}"
                ),
                "OCI",
            ),
            "size": (
                lambda document: document["assets"][0].__setitem__(
                    "size", document["assets"][0]["size"] + 1
                ),
                "size",
            ),
            "hash": (
                lambda document: document["assets"][0].__setitem__(
                    "sha256", "0" * 64
                ),
                "SHA-256",
            ),
            "duplicate": (
                lambda document: document["assets"].append(
                    copy.deepcopy(document["assets"][0])
                ),
                "duplicate",
            ),
        }
        for name, (mutate, error) in mutations.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                dist = Path(temporary)
                release, manifest = self.asset_fixture(dist, digest=None)
                document = json.loads(manifest.read_text(encoding="utf-8"))
                mutate(document)
                manifest.write_text(
                    json.dumps(document, sort_keys=True) + "\n",
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(release_guard.GuardError, error):
                    self.validate_assets(
                        release=release,
                        dist=dist,
                        manifest=manifest,
                    )

    def test_rejects_image_label_drift(self) -> None:
        labels = release_guard.expected_image_labels(
            REPOSITORY, "1.2.3", COMMIT
        )
        labels["org.opencontainers.image.revision"] = "3" * 40
        with self.assertRaisesRegex(release_guard.GuardError, "labels"):
            release_guard.validate_image_labels(
                labels, REPOSITORY, "1.2.3", COMMIT
            )


class WorkflowPolicyTests(unittest.TestCase):
    def test_image_rebuild_uses_tagged_commit_epoch(self) -> None:
        workflow = (
            REPOSITORY_ROOT / ".github/workflows/release.yml"
        ).read_text(encoding="utf-8")
        dockerfile = (REPOSITORY_ROOT / "Dockerfile").read_text(encoding="utf-8")
        self.assertTrue(dockerfile.startswith("# syntax=docker/dockerfile:1.7\n"))
        self.assertIn(
            'source_date_epoch="$(git show -s --format=%ct "$COMMIT")"',
            workflow,
        )
        self.assertIn(
            '--build-arg SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH"',
            workflow,
        )

    def test_temporary_artifact_uploads_are_rerun_safe(self) -> None:
        workflow = (
            REPOSITORY_ROOT / ".github/workflows/release.yml"
        ).read_text(encoding="utf-8")
        self.assertEqual(workflow.count("uses: actions/upload-artifact@"), 2)
        self.assertEqual(workflow.count("          overwrite: true"), 2)

    def test_release_guard_runs_in_pull_request_ci(self) -> None:
        checks = (REPOSITORY_ROOT / "scripts/check.sh").read_text(
            encoding="utf-8"
        )
        ci = (REPOSITORY_ROOT / ".github/workflows/ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("python3 -B scripts/test_release_guard.py", checks)
        self.assertNotIn("python3 scripts/test_release_guard.py", checks)
        self.assertIn("run: ./scripts/check.sh", ci)

    def test_attestation_identity_and_branch_protection_are_token_compatible(
        self,
    ) -> None:
        workflow = (
            REPOSITORY_ROOT / ".github/workflows/release.yml"
        ).read_text(encoding="utf-8")
        self.assertNotIn("/protection", workflow)
        self.assertNotIn("github.server_url", workflow)
        self.assertEqual(
            workflow.count(
                "SIGNER_WORKFLOW: github.com/${{ github.repository }}"
                "/.github/workflows/release.yml"
            ),
            3,
        )

    def test_resume_ancestry_and_latest_order_are_fail_closed(self) -> None:
        workflow = (
            REPOSITORY_ROOT / ".github/workflows/release.yml"
        ).read_text(encoding="utf-8")
        self.assertEqual(workflow.count("git merge-base --is-ancestor"), 4)
        self.assertIn(
            "first-create-default-branch-ref.json",
            workflow,
        )
        decision = workflow.rindex(
            "scripts/release_guard.py decide-stable"
        )
        publication = workflow.rindex("gh api --method PATCH")
        latest = workflow.rindex('--tag "${IMAGE}:latest"')
        immutable_check = workflow.rindex(
            "scripts/release_guard.py validate-checkpoint"
        )
        self.assertLess(decision, publication)
        self.assertLess(publication, immutable_check)
        self.assertLess(immutable_check, latest)
        self.assertIn(
            "A higher stable release exists; preserving both latest pointers.",
            workflow,
        )
        self.assertIn(
            'if [ "$RELEASE_PUBLISHED" != true ]; then',
            workflow,
        )
        self.assertNotIn("should_patch", workflow)
        self.assertIn(
            "latest_args+=(--release-was-published)",
            workflow,
        )

    def test_manifest_is_revalidated_at_every_asset_gate(self) -> None:
        workflow = (
            REPOSITORY_ROOT / ".github/workflows/release.yml"
        ).read_text(encoding="utf-8")
        self.assertEqual(
            workflow.count("scripts/release_guard.py validate-assets"),
            5,
        )
        self.assertGreaterEqual(workflow.count('--tag-object "$TAG_OBJECT"'), 6)
        self.assertIn("work/immediate-assets/${name}", workflow)

    def test_python_release_checks_do_not_write_bytecode(self) -> None:
        workflow = (
            REPOSITORY_ROOT / ".github/workflows/release.yml"
        ).read_text(encoding="utf-8")
        self.assertNotIn("python3 scripts/release_guard.py", workflow)
        self.assertIn("python3 -B scripts/release_guard.py", workflow)


if __name__ == "__main__":
    unittest.main()
