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

for (const [name, value] of Object.entries({
  HTTPFacilitatorClient,
  paymentMiddlewareFromHTTPServer,
  x402HTTPResourceServer,
  x402ResourceServer,
  declarePaymentIdentifierExtension,
  extractPaymentIdentifier,
  ExactNearScheme,
})) {
  if (typeof value !== "function") {
    throw new Error(`${name} is not exported as a function`);
  }
}
if (PAYMENT_IDENTIFIER !== "payment-identifier") {
  throw new Error("unexpected payment-identifier extension key");
}
