use std::{fmt, sync::Arc};

use async_trait::async_trait;
use near_crypto::{PublicKey, Signature, Signer};
use near_primitives::{
    action::Action,
    hash::CryptoHash,
    transaction::{SignedTransaction, Transaction, TransactionV0},
    types::{AccountId, Nonce},
    views::AccessKeyPermissionView,
    views::AccountView,
};
use x402_types::{chain::ChainProviderOps, proto};

use crate::{
    mechanism::verify_proto_request,
    rpc::{NearRpc, NearRpcError, decode_signed_transaction},
    types::{
        NearNetwork, PreparedTransaction, TransactionLookup, VerificationFailure,
        VerificationPolicy, VerifiedPayment,
    },
};

pub trait NearRelayerSigner: Send + Sync {
    fn account_id(&self) -> AccountId;
    fn public_key(&self) -> PublicKey;
    fn sign(&self, bytes: &[u8]) -> Signature;
}

impl NearRelayerSigner for Signer {
    fn account_id(&self) -> AccountId {
        self.get_account_id()
    }

    fn public_key(&self) -> PublicKey {
        Signer::public_key(self)
    }

    fn sign(&self, bytes: &[u8]) -> Signature {
        Signer::sign(self, bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelayerHead {
    pub block_height: u64,
    pub block_hash: CryptoHash,
    pub access_key_nonce: Nonce,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayerStatus {
    pub block_height: u64,
    pub block_hash: CryptoHash,
    pub access_key_nonce: Nonce,
    pub account: AccountView,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettlementDisposition {
    Succeeded {
        transaction: CryptoHash,
    },
    Failed {
        transaction: Option<CryptoHash>,
        reason: String,
        message: Option<String>,
    },
}

#[async_trait]
pub trait NearSettlementCoordinator: Send + Sync {
    async fn settle(
        &self,
        provider: &NearChainProvider,
        payment: VerifiedPayment,
    ) -> Result<SettlementDisposition, NearRpcError>;
}

#[derive(Debug)]
struct SettlementDisabled;

#[async_trait]
impl NearSettlementCoordinator for SettlementDisabled {
    async fn settle(
        &self,
        _provider: &NearChainProvider,
        _payment: VerifiedPayment,
    ) -> Result<SettlementDisposition, NearRpcError> {
        Err(NearRpcError::Request(
            "durable settlement coordinator is not configured".to_owned(),
        ))
    }
}

#[derive(Clone)]
pub struct NearChainProvider {
    network: NearNetwork,
    rpc: Arc<dyn NearRpc>,
    backup_rpc: Option<Arc<dyn NearRpc>>,
    relayer: Arc<dyn NearRelayerSigner>,
    coordinator: Arc<dyn NearSettlementCoordinator>,
}

#[allow(clippy::missing_errors_doc)]
impl NearChainProvider {
    #[must_use]
    pub fn new(
        network: NearNetwork,
        rpc: Arc<dyn NearRpc>,
        relayer: Arc<dyn NearRelayerSigner>,
    ) -> Self {
        Self {
            network,
            rpc,
            backup_rpc: None,
            relayer,
            coordinator: Arc::new(SettlementDisabled),
        }
    }

    #[must_use]
    pub fn with_backup_rpc(mut self, backup_rpc: Arc<dyn NearRpc>) -> Self {
        self.backup_rpc = Some(backup_rpc);
        self
    }

    #[must_use]
    pub fn with_settlement_coordinator(
        mut self,
        coordinator: Arc<dyn NearSettlementCoordinator>,
    ) -> Self {
        self.coordinator = coordinator;
        self
    }

    #[must_use]
    pub const fn network(&self) -> NearNetwork {
        self.network
    }

    #[must_use]
    pub fn relayer_account_id(&self) -> AccountId {
        self.relayer.account_id()
    }

    #[must_use]
    pub fn relayer_public_key(&self) -> PublicKey {
        self.relayer.public_key()
    }

    pub async fn rpc_network_id(&self) -> Result<String, NearRpcError> {
        self.rpc.network_id().await
    }

    pub async fn backup_rpc_network_id(&self) -> Result<String, NearRpcError> {
        let backup = self
            .backup_rpc
            .as_ref()
            .ok_or(NearRpcError::InvalidResponse(
                "backup RPC is not configured",
            ))?;
        backup.network_id().await
    }

    pub async fn rpc_final_block(&self) -> Result<crate::rpc::FinalBlock, NearRpcError> {
        self.rpc.final_block().await
    }

    pub async fn backup_rpc_final_block(&self) -> Result<crate::rpc::FinalBlock, NearRpcError> {
        let backup = self
            .backup_rpc
            .as_ref()
            .ok_or(NearRpcError::InvalidResponse(
                "backup RPC is not configured",
            ))?;
        backup.final_block().await
    }

    pub async fn verify(
        &self,
        request: &proto::VerifyRequest,
        policy: &VerificationPolicy,
    ) -> Result<VerifiedPayment, VerificationFailure> {
        verify_proto_request(self, request, policy).await
    }

    pub async fn relayer_head(&self) -> Result<RelayerHead, NearRpcError> {
        self.relayer_head_from(&self.rpc).await
    }

    pub async fn backup_relayer_head(&self) -> Result<RelayerHead, NearRpcError> {
        let backup = self
            .backup_rpc
            .as_ref()
            .ok_or(NearRpcError::InvalidResponse(
                "backup RPC is not configured",
            ))?;
        self.relayer_head_from(backup).await
    }

    pub async fn relayer_status(&self) -> Result<RelayerStatus, NearRpcError> {
        let block = self.rpc.final_block().await?;
        let account_id = self.relayer.account_id();
        let access_key = self
            .rpc
            .view_access_key(block.hash, account_id.clone(), self.relayer.public_key())
            .await?;
        if !matches!(access_key.permission, AccessKeyPermissionView::FullAccess) {
            return Err(NearRpcError::InvalidResponse(
                "relayer key is not full access",
            ));
        }
        let account = self.rpc.view_account(block.hash, account_id).await?;
        Ok(RelayerStatus {
            block_height: block.height,
            block_hash: block.hash,
            access_key_nonce: access_key.nonce,
            account,
        })
    }

    pub fn prepare_outer_transaction(
        &self,
        payment: &VerifiedPayment,
        relayer_head: RelayerHead,
    ) -> Result<PreparedTransaction, NearRpcError> {
        let relayer_nonce = relayer_head
            .access_key_nonce
            .checked_add(1)
            .ok_or(NearRpcError::InvalidResponse("relayer nonce overflow"))?;
        let signer_id = self.relayer.account_id();
        let signer_public_key = self.relayer.public_key();
        let transaction = Transaction::V0(TransactionV0 {
            signer_id: signer_id.clone(),
            public_key: signer_public_key.clone(),
            nonce: relayer_nonce,
            receiver_id: payment.payer.clone(),
            block_hash: relayer_head.block_hash,
            actions: vec![Action::Delegate(Box::new(
                payment.signed_delegate().clone(),
            ))],
        });
        let (transaction_hash, _) = transaction.get_hash_and_size();
        let signature = self.relayer.sign(transaction_hash.as_ref());
        let signed_transaction = SignedTransaction::new(signature, transaction);
        let signed_transaction_bytes = borsh::to_vec(&signed_transaction)
            .map_err(|_| NearRpcError::InvalidSignedTransaction)?;

        Ok(PreparedTransaction::new(
            transaction_hash,
            relayer_nonce,
            signer_id,
            signer_public_key,
            signed_transaction_bytes,
        ))
    }

    pub async fn broadcast_exact(
        &self,
        signed_transaction_bytes: &[u8],
    ) -> Result<TransactionLookup, NearRpcError> {
        let signed_transaction = decode_signed_transaction(signed_transaction_bytes)?;
        self.rpc.send_transaction_final(signed_transaction).await
    }

    pub async fn query_transaction(
        &self,
        transaction_hash: CryptoHash,
        signer_id: AccountId,
    ) -> Result<TransactionLookup, NearRpcError> {
        self.rpc
            .transaction_status_final(transaction_hash, signer_id)
            .await
    }

    pub async fn query_transaction_backup(
        &self,
        transaction_hash: CryptoHash,
        signer_id: AccountId,
    ) -> Result<TransactionLookup, NearRpcError> {
        let backup = self
            .backup_rpc
            .as_ref()
            .ok_or(NearRpcError::InvalidResponse(
                "backup RPC is not configured",
            ))?;
        backup
            .transaction_status_final(transaction_hash, signer_id)
            .await
    }

    async fn relayer_head_from(&self, rpc: &Arc<dyn NearRpc>) -> Result<RelayerHead, NearRpcError> {
        let block = rpc.final_block().await?;
        let access_key = rpc
            .view_access_key(
                block.hash,
                self.relayer.account_id(),
                self.relayer.public_key(),
            )
            .await?;
        if !matches!(access_key.permission, AccessKeyPermissionView::FullAccess) {
            return Err(NearRpcError::InvalidResponse(
                "relayer key is not full access",
            ));
        }
        Ok(RelayerHead {
            block_height: block.height,
            block_hash: block.hash,
            access_key_nonce: access_key.nonce,
        })
    }

    pub(crate) fn rpc(&self) -> &dyn NearRpc {
        self.rpc.as_ref()
    }

    pub(crate) async fn coordinate_settlement(
        &self,
        payment: VerifiedPayment,
    ) -> Result<SettlementDisposition, NearRpcError> {
        self.coordinator.settle(self, payment).await
    }
}

impl ChainProviderOps for NearChainProvider {
    fn signer_addresses(&self) -> Vec<String> {
        vec![self.relayer.account_id().to_string()]
    }

    fn chain_id(&self) -> x402_types::chain::ChainId {
        self.network.chain_id()
    }
}

impl fmt::Debug for NearChainProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NearChainProvider")
            .field("network", &self.network)
            .field("relayer_account_id", &self.relayer.account_id())
            .field("relayer_public_key", &self.relayer.public_key())
            .field("backup_rpc_configured", &self.backup_rpc.is_some())
            .finish_non_exhaustive()
    }
}
