import assert from "node:assert/strict";
import test from "node:test";

import {
  DeliveryJournal,
  canonicalJson,
  payloadFingerprint,
  workFingerprint,
} from "./journal.mjs";

test("journal prepares work once and grants replay only after settlement", () => {
  const journal = new DeliveryJournal();
  let executions = 0;
  const prepared = journal.prepare("identifier_123456", "payment-a", "work-a", () => {
    executions += 1;
    return { result: "answer" };
  });
  assert.equal(prepared.status, "new");
  assert.equal(prepared.entry.settled, false);

  const retry = journal.prepare("identifier_123456", "payment-a", "work-a", () => {
    executions += 1;
    return { result: "wrong" };
  });
  assert.equal(retry.status, "pending");
  assert.equal(retry.entry.response.result, "answer");
  assert.equal(executions, 1);
  assert.equal(journal.markSettled("identifier_123456", "other-payment"), false);
  assert.equal(journal.markSettled("identifier_123456", "payment-a"), true);

  const replay = journal.inspect("identifier_123456", "work-a");
  assert.equal(replay.status, "settled");
  assert.equal(replay.entry.response.result, "answer");
  assert.equal(journal.inspect("identifier_123456", "work-b").status, "conflict");
  assert.equal(
    journal.prepare("identifier_123456", "payment-b", "work-a", () => ({
      result: "wrong",
    })).status,
    "conflict",
  );
});

test("fingerprints are stable across object key order and bind the work request", () => {
  assert.equal(canonicalJson({ b: 2, a: 1 }), canonicalJson({ a: 1, b: 2 }));
  assert.equal(payloadFingerprint({ b: 2, a: 1 }), payloadFingerprint({ a: 1, b: 2 }));

  const payload = { accepted: { scheme: "exact" }, payload: { signedDelegateAction: "x" } };
  const first = workFingerprint(payload, {
    body: { input: "one" },
    method: "POST",
    path: "/work",
  });
  const second = workFingerprint(payload, {
    body: { input: "two" },
    method: "POST",
    path: "/work",
  });
  assert.notEqual(first, second);
});
