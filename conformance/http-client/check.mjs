import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { HTTPFacilitatorClient } from "@x402/core/server";

const [baseUrl, scenarioPath] = process.argv.slice(2);
if (!baseUrl || !scenarioPath) {
  throw new Error("usage: node check.mjs <base-url> <scenario-json>");
}

const scenario = JSON.parse(await readFile(scenarioPath, "utf8"));

function clientWithKey(apiKey) {
  return new HTTPFacilitatorClient({
    url: baseUrl,
    createAuthHeaders: async () => {
      const protectedHeaders = apiKey ? { "X-API-Key": apiKey } : {};
      return {
        supported: {},
        verify: protectedHeaders,
        settle: protectedHeaders,
      };
    },
  });
}

async function verify(client, request) {
  return client.verify(request.paymentPayload, request.paymentRequirements);
}

async function settle(client, request) {
  return client.settle(request.paymentPayload, request.paymentRequirements);
}

async function expectHttpError(label, action, status, code) {
  try {
    await action();
  } catch (error) {
    assert.ok(error instanceof Error, `${label} did not throw an Error`);
    assert.match(
      error.message,
      new RegExp(`Facilitator (verify|settle) failed \\(${status}\\):`),
      `${label} did not expose HTTP ${status}`,
    );
    assert.match(error.message, new RegExp(`"${code}"`), `${label} omitted ${code}`);
    return;
  }
  throw new Error(`${label} unexpectedly succeeded`);
}

const client = clientWithKey(scenario.apiKey);
const supported = await client.getSupported();
const kind = supported.kinds.find(
  candidate =>
    candidate.x402Version === 2 &&
    candidate.scheme === "exact" &&
    candidate.network === "near:testnet",
);
if (!kind || !supported.extensions.includes("payment-identifier")) {
  throw new Error("official client did not receive the expected NEAR support declaration");
}

const invalidVerification = await verify(client, scenario.invalidVersion);
assert.equal(invalidVerification.isValid, false);
assert.equal(invalidVerification.invalidReason, "invalid_x402_version");

const invalidSettlement = await settle(client, scenario.invalidVersion);
assert.equal(invalidSettlement.success, false);
assert.equal(invalidSettlement.errorReason, "invalid_x402_version");
assert.equal(invalidSettlement.transaction, "");
assert.equal(invalidSettlement.network, "near:testnet");

const validVerification = await verify(client, scenario.valid);
assert.equal(validVerification.isValid, true);
assert.equal(validVerification.payer, scenario.expectedPayer);

const successfulSettlement = await settle(client, scenario.valid);
assert.equal(successfulSettlement.success, true);
assert.equal(successfulSettlement.payer, scenario.expectedPayer);
assert.equal(successfulSettlement.network, "near:testnet");
assert.notEqual(successfulSettlement.transaction, "");

const exactReplay = await settle(client, scenario.valid);
assert.deepEqual(
  exactReplay,
  successfulSettlement,
  "official client did not receive the exact terminal response on replay",
);

await expectHttpError(
  "payment identifier conflict",
  () => settle(client, scenario.conflict),
  409,
  "payment_identifier_conflict",
);

const duplicate = await settle(client, scenario.duplicate);
assert.equal(duplicate.success, false);
assert.equal(duplicate.errorReason, "duplicate_settlement");
assert.equal(duplicate.transaction, "");
assert.equal(duplicate.network, "near:testnet");

const missingAuthClient = clientWithKey();
await expectHttpError(
  "missing verify authentication",
  () => verify(missingAuthClient, scenario.invalidVersion),
  401,
  "invalid_api_key",
);
await expectHttpError(
  "missing settle authentication",
  () => settle(missingAuthClient, scenario.invalidVersion),
  401,
  "invalid_api_key",
);

const invalidAuthClient = clientWithKey(scenario.invalidApiKey);
await expectHttpError(
  "invalid verify authentication",
  () => verify(invalidAuthClient, scenario.invalidVersion),
  401,
  "invalid_api_key",
);
await expectHttpError(
  "invalid settle authentication",
  () => settle(invalidAuthClient, scenario.invalidVersion),
  401,
  "invalid_api_key",
);

const rateClient = clientWithKey(scenario.rateApiKey);
assert.equal((await verify(rateClient, scenario.invalidVersion)).isValid, false);
assert.equal((await verify(rateClient, scenario.invalidVersion)).isValid, false);
await expectHttpError(
  "verify rate limit",
  () => verify(rateClient, scenario.invalidVersion),
  429,
  "rate_limit_exceeded",
);

process.stdout.write(
  JSON.stringify({
    supported: true,
    invalidVersion: {
      verify: invalidVerification.invalidReason,
      settle: invalidSettlement.errorReason,
    },
    valid: {
      verify: validVerification.isValid,
      settle: successfulSettlement.success,
      replay: true,
    },
    conflict: 409,
    duplicate: duplicate.errorReason,
    authentication: true,
    rateLimit: 429,
  }),
);
