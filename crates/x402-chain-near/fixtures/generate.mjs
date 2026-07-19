import { readFileSync } from "node:fs";
import { KeyPair } from "@near-js/crypto";
import { KeyPairSigner } from "@near-js/signers";
import {
  actionCreators,
  buildDelegateAction,
  encodeSignedDelegate,
} from "@near-js/transactions";
import { DEFAULT_FT_TRANSFER_GAS, ONE_YOCTO } from "@x402/near";

const WARNING = "DETERMINISTIC TEST KEYS — DO NOT FUND";

// These private keys are public test vectors. They must never control funds.
const TEST_KEYS_DO_NOT_FUND = [
  {
    curve: "ed25519",
    secretKey:
      "ed25519:4m8u95BQAFnA3c593fnghrApJ9c4bufLydgdUwaHnmHvRs3r5ukT68H2punoN6Mg45MnRGnH5AEQcjQGnaNPJoQu",
  },
  {
    curve: "secp256k1",
    secretKey:
      "secp256k1:AkNtHhiTUSWkVFVE6d2TuPvCy2Y4aajodW3fGAzq8rdmNWmxsaMkYfgKwTwyc1X2qeXvUFiEygf6RKk3KKgiVGYEYVyqdsRqeDkPYGFoSPFFKN91w3aAWsMMZ5xBF9zmqFk",
  },
];

const common = {
  senderId: "alice.testnet",
  receiverId: "usdc.testnet",
  payTo: "merchant.testnet",
  amount: "1000000",
  nonce: "5",
  maxBlockHeight: "1060",
  gas: DEFAULT_FT_TRANSFER_GAS.toString(),
  deposit: ONE_YOCTO.toString(),
};

async function buildFixture({ curve, secretKey }) {
  const keyPair = KeyPair.fromString(secretKey);
  const signer = new KeyPairSigner(keyPair);
  const publicKey = keyPair.getPublicKey();
  const delegateAction = buildDelegateAction({
    actions: [
      actionCreators.functionCall(
        "ft_transfer",
        { receiver_id: common.payTo, amount: common.amount },
        DEFAULT_FT_TRANSFER_GAS,
        ONE_YOCTO,
      ),
    ],
    maxBlockHeight: BigInt(common.maxBlockHeight),
    nonce: BigInt(common.nonce),
    publicKey,
    receiverId: common.receiverId,
    senderId: common.senderId,
  });
  const [, signedDelegate] = await signer.signDelegateAction(delegateAction);
  return {
    curve,
    publicKey: publicKey.toString(),
    signedDelegateAction: Buffer.from(
      encodeSignedDelegate(signedDelegate),
    ).toString("base64"),
  };
}

const output = {
  warning: WARNING,
  generatedWith: "@x402/near@2.19.0",
  ...common,
  delegates: await Promise.all(TEST_KEYS_DO_NOT_FUND.map(buildFixture)),
};
const serialized = `${JSON.stringify(output, null, 2)}\n`;

if (process.argv.includes("--check")) {
  const expected = readFileSync(
    new URL("./signed-delegates.json", import.meta.url),
    "utf8",
  );
  if (serialized !== expected) {
    console.error(
      "signed-delegates.json differs; run `npm run generate > signed-delegates.json`",
    );
    process.exitCode = 1;
  }
} else {
  process.stdout.write(serialized);
}
