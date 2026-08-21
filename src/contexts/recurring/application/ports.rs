use crate::contexts::ledger::public::{AnnotationResult, LedgerError, UpdateTransactionAnnotation};
use std::future::Future;
pub(crate) trait AnnotateLedger: Send + Sync {
    fn annotate(
        &self,
        command: UpdateTransactionAnnotation,
    ) -> impl Future<Output = Result<AnnotationResult, LedgerError>> + Send;
}
