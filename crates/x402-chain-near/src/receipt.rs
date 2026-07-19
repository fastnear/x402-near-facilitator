use std::collections::{HashMap, HashSet, VecDeque};

use near_primitives::{
    hash::CryptoHash,
    types::AccountId,
    views::{
        ExecutionOutcomeWithIdView, ExecutionStatusView, FinalExecutionOutcomeView,
        FinalExecutionStatus,
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuccessfulTransferReceipt {
    pub receipt_id: CryptoHash,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReceiptValidationError {
    #[error("final execution has not started")]
    NotStarted,
    #[error("final execution is still pending")]
    Pending,
    #[error("final execution failed: {0}")]
    FinalFailure(String),
    #[error("transaction outcome failed: {0}")]
    TransactionFailure(String),
    #[error("outer transaction did not create exactly one delegate receipt")]
    InvalidDelegateReceiptCount,
    #[error("delegate receipt outcome is missing")]
    MissingDelegateReceipt,
    #[error("delegate receipt executed on an unexpected account")]
    InvalidDelegateExecutor,
    #[error("delegate receipt failed: {0}")]
    DelegateFailure(String),
    #[error("receipt graph contains a missing outcome")]
    IncompleteReceiptGraph,
    #[error("receipt graph contains duplicate outcome identifiers")]
    DuplicateReceiptOutcome,
    #[error("receipt graph contains a cycle")]
    CyclicReceiptGraph,
    #[error("reachable receipt failed: {0}")]
    ReachableReceiptFailure(String),
    #[error("delegate receipt did not create exactly one token-contract receipt")]
    InvalidTokenReceiptCount,
    #[error("token receipt was not successful")]
    TokenReceiptNotSuccessful,
    #[error("token receipt failed: {0}")]
    TokenReceiptFailure(String),
    #[error("final outcome transaction hash does not match the submitted transaction")]
    TransactionHashMismatch,
    #[error("transaction outcome ID does not match the submitted transaction")]
    TransactionOutcomeIdMismatch,
    #[error("final outcome transaction signer does not match the relayer")]
    TransactionSignerMismatch,
    #[error("transaction outcome executed on an unexpected account")]
    InvalidTransactionExecutor,
    #[error("final outcome transaction receiver does not match the payer")]
    TransactionReceiverMismatch,
}

impl ReceiptValidationError {
    /// Whether the error is authoritative evidence that the submitted payment
    /// failed on chain rather than incomplete or inconsistent RPC evidence.
    #[must_use]
    pub const fn is_definitive_failure(&self) -> bool {
        matches!(
            self,
            Self::FinalFailure(_)
                | Self::TransactionFailure(_)
                | Self::DelegateFailure(_)
                | Self::ReachableReceiptFailure(_)
                | Self::TokenReceiptFailure(_)
        )
    }
}

fn execution_failure(status: &ExecutionStatusView) -> Option<String> {
    match status {
        ExecutionStatusView::Failure(error) => Some(format!("{error:?}")),
        _ => None,
    }
}

fn by_id(
    outcomes: &[ExecutionOutcomeWithIdView],
) -> Result<HashMap<CryptoHash, &ExecutionOutcomeWithIdView>, ReceiptValidationError> {
    let mut indexed = HashMap::with_capacity(outcomes.len());
    for outcome in outcomes {
        if indexed.insert(outcome.id, outcome).is_some() {
            return Err(ReceiptValidationError::DuplicateReceiptOutcome);
        }
    }
    Ok(indexed)
}

/// Validates the exact transaction → delegate receipt → token receipt graph.
///
/// The outer transaction's final success is insufficient: the direct token
/// receipt spawned by the payer's delegate receipt must finish with
/// `SuccessValue`.
///
/// # Errors
///
/// Returns a typed validation error whenever the outcome is nonfinal, failed,
/// malformed, incomplete, cyclic, or does not contain exactly one successful
/// direct token-contract receipt.
#[allow(clippy::too_many_lines)]
pub fn interpret_final_outcome(
    outcome: &FinalExecutionOutcomeView,
    payer: &AccountId,
    asset: &AccountId,
) -> Result<SuccessfulTransferReceipt, ReceiptValidationError> {
    match &outcome.status {
        FinalExecutionStatus::NotStarted => return Err(ReceiptValidationError::NotStarted),
        FinalExecutionStatus::Started => return Err(ReceiptValidationError::Pending),
        FinalExecutionStatus::Failure(error) => {
            return Err(ReceiptValidationError::FinalFailure(format!("{error:?}")));
        }
        FinalExecutionStatus::SuccessValue(_) => {}
    }

    if let Some(error) = execution_failure(&outcome.transaction_outcome.outcome.status) {
        return Err(ReceiptValidationError::TransactionFailure(error));
    }
    if matches!(
        outcome.transaction_outcome.outcome.status,
        ExecutionStatusView::Unknown
    ) {
        return Err(ReceiptValidationError::IncompleteReceiptGraph);
    }

    let transaction_children = &outcome.transaction_outcome.outcome.receipt_ids;
    if transaction_children.len() != 1 {
        return Err(ReceiptValidationError::InvalidDelegateReceiptCount);
    }

    let outcomes = by_id(&outcome.receipts_outcome)?;
    let delegate = outcomes
        .get(&transaction_children[0])
        .ok_or(ReceiptValidationError::MissingDelegateReceipt)?;
    if delegate.outcome.executor_id != *payer {
        return Err(ReceiptValidationError::InvalidDelegateExecutor);
    }
    if let Some(error) = execution_failure(&delegate.outcome.status) {
        return Err(ReceiptValidationError::DelegateFailure(error));
    }
    if matches!(delegate.outcome.status, ExecutionStatusView::Unknown) {
        return Err(ReceiptValidationError::IncompleteReceiptGraph);
    }

    if delegate
        .outcome
        .receipt_ids
        .iter()
        .any(|receipt_id| !outcomes.contains_key(receipt_id))
    {
        return Err(ReceiptValidationError::IncompleteReceiptGraph);
    }
    let token_receipts = delegate
        .outcome
        .receipt_ids
        .iter()
        .filter_map(|receipt_id| outcomes.get(receipt_id))
        .filter(|receipt| receipt.outcome.executor_id == *asset)
        .collect::<Vec<_>>();
    if token_receipts.len() != 1 {
        return Err(ReceiptValidationError::InvalidTokenReceiptCount);
    }
    let token = token_receipts[0];

    let mut queue = VecDeque::from(transaction_children.clone());
    let mut visited = HashSet::new();
    while let Some(receipt_id) = queue.pop_front() {
        if !visited.insert(receipt_id) {
            continue;
        }
        let receipt = outcomes
            .get(&receipt_id)
            .ok_or(ReceiptValidationError::IncompleteReceiptGraph)?;
        if let Some(error) = execution_failure(&receipt.outcome.status) {
            return Err(ReceiptValidationError::ReachableReceiptFailure(error));
        }
        if receipt.id != token.id && matches!(receipt.outcome.status, ExecutionStatusView::Unknown)
        {
            return Err(ReceiptValidationError::IncompleteReceiptGraph);
        }
        queue.extend(receipt.outcome.receipt_ids.iter().copied());
    }

    let mut indegrees = visited
        .iter()
        .copied()
        .map(|receipt_id| (receipt_id, 0_usize))
        .collect::<HashMap<_, _>>();
    for receipt_id in &visited {
        let receipt = outcomes
            .get(receipt_id)
            .ok_or(ReceiptValidationError::IncompleteReceiptGraph)?;
        for child_id in &receipt.outcome.receipt_ids {
            if let Some(indegree) = indegrees.get_mut(child_id) {
                *indegree = indegree.saturating_add(1);
            }
        }
    }
    let mut roots = indegrees
        .iter()
        .filter_map(|(receipt_id, indegree)| (*indegree == 0).then_some(*receipt_id))
        .collect::<VecDeque<_>>();
    let mut sorted = 0_usize;
    while let Some(receipt_id) = roots.pop_front() {
        sorted = sorted.saturating_add(1);
        let receipt = outcomes
            .get(&receipt_id)
            .ok_or(ReceiptValidationError::IncompleteReceiptGraph)?;
        for child_id in &receipt.outcome.receipt_ids {
            let Some(indegree) = indegrees.get_mut(child_id) else {
                continue;
            };
            *indegree = indegree.saturating_sub(1);
            if *indegree == 0 {
                roots.push_back(*child_id);
            }
        }
    }
    if sorted != visited.len() {
        return Err(ReceiptValidationError::CyclicReceiptGraph);
    }

    match &token.outcome.status {
        ExecutionStatusView::SuccessValue(value) => Ok(SuccessfulTransferReceipt {
            receipt_id: token.id,
            value: value.clone(),
        }),
        ExecutionStatusView::Failure(error) => Err(ReceiptValidationError::TokenReceiptFailure(
            format!("{error:?}"),
        )),
        ExecutionStatusView::Unknown | ExecutionStatusView::SuccessReceiptId(_) => {
            Err(ReceiptValidationError::TokenReceiptNotSuccessful)
        }
    }
}

/// Binds an RPC final outcome to the locally prepared outer transaction.
///
/// A receipt graph is not trusted until the RPC response names the exact
/// transaction hash, signer, receiver, transaction-outcome ID, and outer
/// executor expected by the service.
///
/// # Errors
///
/// Returns a typed indeterminate validation error when any identity field
/// disagrees with the locally prepared transaction.
pub fn validate_final_outcome_identity(
    outcome: &FinalExecutionOutcomeView,
    expected_hash: CryptoHash,
    expected_signer: &AccountId,
    expected_receiver: &AccountId,
) -> Result<(), ReceiptValidationError> {
    if outcome.transaction.hash != expected_hash {
        return Err(ReceiptValidationError::TransactionHashMismatch);
    }
    if outcome.transaction_outcome.id != expected_hash {
        return Err(ReceiptValidationError::TransactionOutcomeIdMismatch);
    }
    if outcome.transaction.signer_id != *expected_signer {
        return Err(ReceiptValidationError::TransactionSignerMismatch);
    }
    if outcome.transaction.receiver_id != *expected_receiver {
        return Err(ReceiptValidationError::TransactionReceiverMismatch);
    }
    if outcome.transaction_outcome.outcome.executor_id != *expected_signer {
        return Err(ReceiptValidationError::InvalidTransactionExecutor);
    }
    Ok(())
}
