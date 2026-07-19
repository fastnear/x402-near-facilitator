import { createHash } from "node:crypto";
import { constants } from "node:fs";
import { open } from "node:fs/promises";

import { HTTPFacilitatorClient } from "@x402/core/server";
import {
  paymentMiddlewareFromHTTPServer,
  x402HTTPResourceServer,
  x402ResourceServer,
} from "@x402/express";
import {
  PAYMENT_IDENTIFIER,
  declarePaymentIdentifierExtension,
  extractPaymentIdentifier,
} from "@x402/extensions/payment-identifier";
import { ExactNearScheme } from "@x402/near/exact/server";
import express from "express";

import {
  DeliveryJournal,
  canonicalJson,
  payloadFingerprint,
  workFingerprint,
} from "./journal.mjs";

const facilitatorUrl = requiredEnvironment("FACILITATOR_URL");
const apiKey = await readCredential(requiredEnvironment("FACILITATOR_API_KEY_FILE"));
const network = requiredEnvironment("NETWORK");
const asset = requiredEnvironment("ASSET");
const payTo = requiredEnvironment("PAY_TO");
const amount = process.env.AMOUNT ?? "1000";
const port = parsePort(process.env.PORT ?? "4021");

if (!["near:testnet", "near:mainnet"].includes(network)) {
  throw new Error("NETWORK must be near:testnet or near:mainnet");
}
if (!/^[0-9]+$/.test(amount) || BigInt(amount) < 1000n) {
  throw new Error("AMOUNT must be at least 1000 atomic USDC");
}

const facilitator = new HTTPFacilitatorClient({
  url: facilitatorUrl,
  createAuthHeaders: async () => ({
    supported: {},
    verify: { "X-API-Key": apiKey },
    settle: { "X-API-Key": apiKey },
  }),
});

const route = "POST /work";
const routes = {
  [route]: {
    accepts: [
      {
        scheme: "exact",
        price: { asset, amount },
        network,
        payTo,
      },
    ],
    description: "Deterministic paid work with independent delivery deduplication",
    mimeType: "application/json",
    extensions: {
      [PAYMENT_IDENTIFIER]: declarePaymentIdentifierExtension(true),
    },
  },
};

// Development-only journal. Production must use durable transactional storage.
const deliveries = new DeliveryJournal();
const resourceServer = new x402ResourceServer(facilitator)
  .register(network, new ExactNearScheme())
  .onAfterSettle(async ({ paymentPayload, result }) => {
    if (!result.success) {
      return;
    }
    const identifier = extractPaymentIdentifier(paymentPayload);
    if (!identifier) {
      throw new Error("payment-identifier is required for paid work");
    }
    if (!deliveries.markSettled(identifier, payloadFingerprint(paymentPayload))) {
      throw new Error("settlement succeeded without a matching delivery-journal entry");
    }
  });

const httpServer = new x402HTTPResourceServer(resourceServer, routes).onProtectedRequest(
  async context => {
    if (!context.paymentHeader) {
      return;
    }
    let paymentPayload;
    try {
      paymentPayload = JSON.parse(Buffer.from(context.paymentHeader, "base64").toString("utf8"));
    } catch {
      // Leave malformed payload handling to the official x402 middleware.
      return;
    }
    const identifier = extractPaymentIdentifier(paymentPayload);
    if (!identifier) {
      return;
    }
    const request = context.adapter.req;
    request.x402PaymentIdentifier = identifier;
    request.x402PaymentFingerprint = payloadFingerprint(paymentPayload);
    request.x402WorkFingerprint = workFingerprint(paymentPayload, request);
    const observed = deliveries.prepare(
      identifier,
      request.x402PaymentFingerprint,
      request.x402WorkFingerprint,
      () => ({
        result: createHash("sha256").update(canonicalJson(request.body)).digest("hex"),
      }),
    );
    if (observed.status === "conflict") {
      request.x402PaymentConflict = true;
      return { grantAccess: true };
    }
    if (observed.status === "settled") {
      request.x402PaymentReplay = true;
      return { grantAccess: true };
    }
    if (observed.status === "new") {
      request.x402DeliveryWasNew = true;
    }
    if (observed.status === "pending") {
      request.x402PaymentRetry = true;
    }
  },
);

const app = express();
app.use(express.json({ limit: "16kb", strict: true }));
app.use(paymentMiddlewareFromHTTPServer(httpServer));
app.post("/work", (request, response) => {
  const identifier = request.x402PaymentIdentifier;
  if (!identifier) {
    response.status(500).json({ error: "payment identifier was not bound to request" });
    return;
  }
  if (request.x402PaymentConflict) {
    response.status(409).json({ error: "payment identifier already used for other work" });
    return;
  }
  const prepared = deliveries.prepare(
    identifier,
    request.x402PaymentFingerprint,
    request.x402WorkFingerprint,
    () => ({
      result: createHash("sha256").update(canonicalJson(request.body)).digest("hex"),
    }),
  );
  if (prepared.status === "conflict") {
    response.status(409).json({ error: "payment identifier already used for other work" });
    return;
  }
  response.json({
    ...prepared.entry.response,
    replayed: !request.x402DeliveryWasNew || Boolean(request.x402PaymentReplay),
  });
});

app.listen(port, "127.0.0.1", () => {
  console.log(`NEAR x402 reference workload listening on http://127.0.0.1:${port}`);
});

function requiredEnvironment(name) {
  const value = process.env[name];
  if (!value) {
    throw new Error(`${name} is required`);
  }
  return value;
}

async function readCredential(path) {
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const metadata = await handle.stat();
    if (!metadata.isFile() || (metadata.mode & 0o077) !== 0) {
      throw new Error("facilitator API key file must be a mode-0600 regular file");
    }
    const value = await handle.readFile("utf8");
    if (!value.endsWith("\n") || value.endsWith("\n\n")) {
      throw new Error("facilitator API key file must end with exactly one newline");
    }
    const key = value.slice(0, -1);
    if (
      key.trim() !== key ||
      !/^x402_(?:test|live)_[0-9a-f]{24}\.[0-9a-f]{64}$/.test(key)
    ) {
      throw new Error("facilitator API key file has an invalid value");
    }
    return key;
  } finally {
    await handle.close();
  }
}

function parsePort(value) {
  if (!/^[0-9]+$/.test(value)) {
    throw new Error("PORT must be an integer");
  }
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || parsed > 65535) {
    throw new Error("PORT is out of range");
  }
  return parsed;
}
