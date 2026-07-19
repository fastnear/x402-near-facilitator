import { createHash } from "node:crypto";

/**
 * Development-only in-memory delivery journal.
 *
 * A production resource server must replace this with durable transactional
 * storage. The facilitator's idempotency journal prevents a second payment;
 * this journal independently prevents a paid request from executing work
 * more than once.
 */
export class DeliveryJournal {
  #entries = new Map();

  inspect(identifier, workFingerprint, paymentFingerprint) {
    const entry = this.#entries.get(identifier);
    if (!entry) {
      return { status: "new" };
    }
    if (
      entry.workFingerprint !== workFingerprint ||
      (paymentFingerprint !== undefined && entry.paymentFingerprint !== paymentFingerprint)
    ) {
      return { status: "conflict" };
    }
    return {
      status: entry.settled ? "settled" : "pending",
      entry,
    };
  }

  prepare(identifier, paymentFingerprint, workFingerprint, makeResponse) {
    const observed = this.inspect(identifier, workFingerprint, paymentFingerprint);
    if (observed.status !== "new") {
      return observed;
    }
    const entry = {
      paymentFingerprint,
      workFingerprint,
      response: makeResponse(),
      settled: false,
    };
    this.#entries.set(identifier, entry);
    return { status: "new", entry };
  }

  markSettled(identifier, paymentFingerprint) {
    const entry = this.#entries.get(identifier);
    if (!entry || entry.paymentFingerprint !== paymentFingerprint) {
      return false;
    }
    entry.settled = true;
    return true;
  }
}

export function payloadFingerprint(payload) {
  return createHash("sha256").update(canonicalJson(payload)).digest("hex");
}

export function workFingerprint(paymentPayload, request) {
  return createHash("sha256")
    .update(
      canonicalJson({
        body: request.body,
        method: request.method,
        path: request.path,
        paymentPayload,
      }),
    )
    .digest("hex");
}

export function canonicalJson(value) {
  if (value === undefined) {
    return "null";
  }
  if (value === null || typeof value !== "object") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(canonicalJson).join(",")}]`;
  }
  return `{${Object.keys(value)
    .sort()
    .map(key => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
    .join(",")}}`;
}
