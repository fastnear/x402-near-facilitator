use std::time::Duration;

use async_trait::async_trait;
use borsh::BorshDeserialize;
use near_crypto::PublicKey;
use near_jsonrpc_client::{
    JsonRpcClient,
    errors::{JsonRpcError, JsonRpcServerError},
    methods,
};
use near_jsonrpc_primitives::types::{
    query::{QueryResponseKind, RpcQueryError},
    transactions::{RpcTransactionError, TransactionInfo},
};
use near_primitives::{
    errors::{FunctionCallError, InvalidTxError, MethodResolveError},
    hash::CryptoHash,
    transaction::SignedTransaction,
    types::{AccountId, BlockId, BlockReference, Finality, FunctionArgs},
    views::{
        AccessKeyView, AccountView, FinalExecutionOutcomeViewEnum, QueryRequest, TxExecutionStatus,
    },
};

use crate::types::TransactionLookup;

const RPC_CALL_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalBlock {
    pub height: u64,
    pub hash: CryptoHash,
}

#[derive(Debug, thiserror::Error)]
pub enum NearRpcError {
    #[error("account not found")]
    AccountNotFound,
    #[error("access key not found")]
    AccessKeyNotFound,
    #[error("contract method not found")]
    MethodNotFound,
    #[error("transaction is unknown")]
    TransactionUnknown,
    #[error("transaction was definitively rejected")]
    TransactionRejected,
    #[error("transaction was temporarily rejected")]
    TransactionTemporarilyRejected,
    #[error("RPC request timed out")]
    Timeout,
    #[error("invalid RPC response: {0}")]
    InvalidResponse(&'static str),
    #[error("invalid signed transaction bytes")]
    InvalidSignedTransaction,
    #[error("RPC request failed: {0}")]
    Request(String),
}

#[async_trait]
pub trait NearRpc: Send + Sync {
    async fn network_id(&self) -> Result<String, NearRpcError>;

    async fn final_block(&self) -> Result<FinalBlock, NearRpcError>;

    async fn view_account(
        &self,
        block_hash: CryptoHash,
        account_id: AccountId,
    ) -> Result<AccountView, NearRpcError>;

    async fn view_access_key(
        &self,
        block_hash: CryptoHash,
        account_id: AccountId,
        public_key: PublicKey,
    ) -> Result<AccessKeyView, NearRpcError>;

    async fn call_function(
        &self,
        block_hash: CryptoHash,
        contract_id: AccountId,
        method_name: String,
        args: Vec<u8>,
    ) -> Result<Vec<u8>, NearRpcError>;

    async fn send_transaction_final(
        &self,
        signed_transaction: SignedTransaction,
    ) -> Result<TransactionLookup, NearRpcError>;

    async fn transaction_status_final(
        &self,
        transaction_hash: CryptoHash,
        signer_id: AccountId,
    ) -> Result<TransactionLookup, NearRpcError>;
}

pub struct JsonRpcNearRpc {
    client: JsonRpcClient,
}

impl JsonRpcNearRpc {
    #[must_use]
    pub fn connect(url: &str) -> Self {
        Self {
            client: JsonRpcClient::connect(url),
        }
    }

    fn pinned(block_hash: CryptoHash) -> BlockReference {
        BlockReference::BlockId(BlockId::Hash(block_hash))
    }
}

impl std::fmt::Debug for JsonRpcNearRpc {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JsonRpcNearRpc(<redacted>)")
    }
}

fn query_error(error: JsonRpcError<RpcQueryError>) -> NearRpcError {
    match error {
        JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
            RpcQueryError::UnknownAccount { .. },
        )) => NearRpcError::AccountNotFound,
        JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
            RpcQueryError::UnknownAccessKey { .. },
        )) => NearRpcError::AccessKeyNotFound,
        JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
            RpcQueryError::ContractExecutionError {
                error: FunctionCallError::MethodResolveError(MethodResolveError::MethodNotFound),
                ..
            },
        )) => NearRpcError::MethodNotFound,
        other => NearRpcError::Request(other.to_string()),
    }
}

fn transaction_error(error: JsonRpcError<RpcTransactionError>) -> NearRpcError {
    match error {
        JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
            RpcTransactionError::UnknownTransaction { .. },
        )) => NearRpcError::TransactionUnknown,
        JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
            RpcTransactionError::InvalidTransaction { context },
        )) => classify_invalid_transaction(&context),
        JsonRpcError::ServerError(JsonRpcServerError::HandlerError(
            RpcTransactionError::TimeoutError,
        )) => NearRpcError::Timeout,
        other => NearRpcError::Request(other.to_string()),
    }
}

fn classify_invalid_transaction(error: &InvalidTxError) -> NearRpcError {
    // Rejections derived from mutable chain state remain indeterminate after
    // `submitted`: another RPC may already know the exact transaction, or the
    // same bytes may become admissible before their block hash expires.
    match error {
        InvalidTxError::InvalidSignerId { .. }
        | InvalidTxError::InvalidReceiverId { .. }
        | InvalidTxError::InvalidSignature
        | InvalidTxError::CostOverflow
        | InvalidTxError::ActionsValidation(_)
        | InvalidTxError::TransactionSizeExceeded { .. }
        | InvalidTxError::InvalidTransactionVersion
        | InvalidTxError::InvalidNonceIndex { .. } => NearRpcError::TransactionRejected,
        InvalidTxError::InvalidAccessKeyError(_)
        | InvalidTxError::SignerDoesNotExist { .. }
        | InvalidTxError::InvalidNonce { .. }
        | InvalidTxError::NonceTooLarge { .. }
        | InvalidTxError::NotEnoughBalance { .. }
        | InvalidTxError::LackBalanceForState { .. }
        | InvalidTxError::InvalidChain
        | InvalidTxError::Expired
        | InvalidTxError::StorageError(_)
        | InvalidTxError::ShardCongested { .. }
        | InvalidTxError::ShardStuck { .. }
        | InvalidTxError::NotEnoughGasKeyBalance { .. }
        | InvalidTxError::NotEnoughBalanceForDeposit { .. } => {
            NearRpcError::TransactionTemporarilyRejected
        }
    }
}

fn classify_transaction(
    response: methods::tx::RpcTransactionResponse,
) -> Result<TransactionLookup, NearRpcError> {
    if response.final_execution_status != TxExecutionStatus::Final {
        return Ok(TransactionLookup::Pending(response.final_execution_status));
    }
    let outcome = response
        .final_execution_outcome
        .ok_or(NearRpcError::InvalidResponse(
            "FINAL transaction response has no execution outcome",
        ))?;
    let outcome = match outcome {
        FinalExecutionOutcomeViewEnum::FinalExecutionOutcome(outcome) => outcome,
        FinalExecutionOutcomeViewEnum::FinalExecutionOutcomeWithReceipt(outcome) => {
            outcome.final_outcome
        }
    };
    Ok(TransactionLookup::Final(Box::new(outcome)))
}

#[async_trait]
impl NearRpc for JsonRpcNearRpc {
    async fn network_id(&self) -> Result<String, NearRpcError> {
        tokio::time::timeout(
            RPC_CALL_TIMEOUT,
            self.client.call(methods::status::RpcStatusRequest),
        )
        .await
        .map_err(|_| NearRpcError::Timeout)?
        .map(|response| response.chain_id)
        .map_err(|error| NearRpcError::Request(error.to_string()))
    }

    async fn final_block(&self) -> Result<FinalBlock, NearRpcError> {
        let response = tokio::time::timeout(
            RPC_CALL_TIMEOUT,
            self.client.call(methods::block::RpcBlockRequest {
                block_reference: BlockReference::Finality(Finality::Final),
            }),
        )
        .await
        .map_err(|_| NearRpcError::Timeout)?
        .map_err(|error| NearRpcError::Request(error.to_string()))?;
        Ok(FinalBlock {
            height: response.header.height,
            hash: response.header.hash,
        })
    }

    async fn view_account(
        &self,
        block_hash: CryptoHash,
        account_id: AccountId,
    ) -> Result<AccountView, NearRpcError> {
        let response = tokio::time::timeout(
            RPC_CALL_TIMEOUT,
            self.client.call(methods::query::RpcQueryRequest {
                block_reference: Self::pinned(block_hash),
                request: QueryRequest::ViewAccount { account_id },
            }),
        )
        .await
        .map_err(|_| NearRpcError::Timeout)?
        .map_err(query_error)?;
        match response.kind {
            QueryResponseKind::ViewAccount(account) => Ok(account),
            _ => Err(NearRpcError::InvalidResponse(
                "view_account returned another response kind",
            )),
        }
    }

    async fn view_access_key(
        &self,
        block_hash: CryptoHash,
        account_id: AccountId,
        public_key: PublicKey,
    ) -> Result<AccessKeyView, NearRpcError> {
        let response = tokio::time::timeout(
            RPC_CALL_TIMEOUT,
            self.client.call(methods::query::RpcQueryRequest {
                block_reference: Self::pinned(block_hash),
                request: QueryRequest::ViewAccessKey {
                    account_id,
                    public_key,
                },
            }),
        )
        .await
        .map_err(|_| NearRpcError::Timeout)?
        .map_err(query_error)?;
        match response.kind {
            QueryResponseKind::AccessKey(access_key) => Ok(access_key),
            _ => Err(NearRpcError::InvalidResponse(
                "view_access_key returned another response kind",
            )),
        }
    }

    async fn call_function(
        &self,
        block_hash: CryptoHash,
        contract_id: AccountId,
        method_name: String,
        args: Vec<u8>,
    ) -> Result<Vec<u8>, NearRpcError> {
        let response = tokio::time::timeout(
            RPC_CALL_TIMEOUT,
            self.client.call(methods::query::RpcQueryRequest {
                block_reference: Self::pinned(block_hash),
                request: QueryRequest::CallFunction {
                    account_id: contract_id,
                    method_name,
                    args: FunctionArgs::from(args),
                },
            }),
        )
        .await
        .map_err(|_| NearRpcError::Timeout)?
        .map_err(query_error)?;
        match response.kind {
            QueryResponseKind::CallResult(result) => Ok(result.result),
            _ => Err(NearRpcError::InvalidResponse(
                "call_function returned another response kind",
            )),
        }
    }

    async fn send_transaction_final(
        &self,
        signed_transaction: SignedTransaction,
    ) -> Result<TransactionLookup, NearRpcError> {
        let response = tokio::time::timeout(
            RPC_CALL_TIMEOUT,
            self.client
                .call(methods::send_tx::RpcSendTransactionRequest {
                    signed_transaction,
                    wait_until: TxExecutionStatus::Final,
                }),
        )
        .await
        .map_err(|_| NearRpcError::Timeout)?
        .map_err(transaction_error)?;
        classify_transaction(response)
    }

    async fn transaction_status_final(
        &self,
        transaction_hash: CryptoHash,
        signer_id: AccountId,
    ) -> Result<TransactionLookup, NearRpcError> {
        let response = tokio::time::timeout(
            RPC_CALL_TIMEOUT,
            self.client.call(methods::tx::RpcTransactionStatusRequest {
                transaction_info: TransactionInfo::TransactionId {
                    tx_hash: transaction_hash,
                    sender_account_id: signer_id,
                },
                wait_until: TxExecutionStatus::Final,
            }),
        )
        .await
        .map_err(|_| NearRpcError::Timeout)?;
        match response {
            Ok(response) => classify_transaction(response),
            Err(error) => match transaction_error(error) {
                NearRpcError::TransactionUnknown => Ok(TransactionLookup::Unknown),
                other => Err(other),
            },
        }
    }
}

#[doc(hidden)]
pub fn decode_signed_transaction(bytes: &[u8]) -> Result<SignedTransaction, NearRpcError> {
    let transaction = SignedTransaction::try_from_slice(bytes)
        .map_err(|_| NearRpcError::InvalidSignedTransaction)?;
    if !transaction.signature.verify(
        transaction.get_hash().as_ref(),
        transaction.transaction.public_key(),
    ) {
        return Err(NearRpcError::InvalidSignedTransaction);
    }
    Ok(transaction)
}

/// Strictly decodes stored signed-transaction bytes and returns the NEAR
/// transaction hash committed to by those bytes.
///
/// `try_from_slice` rejects trailing bytes, so callers can safely compare the
/// returned hash with a separately journaled hash before rebroadcast.
///
/// # Errors
///
/// Returns [`NearRpcError::InvalidSignedTransaction`] for malformed,
/// truncated, or non-canonical bytes.
pub fn signed_transaction_hash(bytes: &[u8]) -> Result<CryptoHash, NearRpcError> {
    Ok(decode_signed_transaction(bytes)?.get_hash())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutable_state_transaction_rejections_remain_indeterminate() {
        assert!(matches!(
            classify_invalid_transaction(&InvalidTxError::InvalidNonce {
                tx_nonce: 7,
                ak_nonce: 7,
            }),
            NearRpcError::TransactionTemporarilyRejected
        ));
        assert!(matches!(
            classify_invalid_transaction(&InvalidTxError::Expired),
            NearRpcError::TransactionTemporarilyRejected
        ));
        assert!(matches!(
            classify_invalid_transaction(&InvalidTxError::InvalidSignature),
            NearRpcError::TransactionRejected
        ));
    }
}
