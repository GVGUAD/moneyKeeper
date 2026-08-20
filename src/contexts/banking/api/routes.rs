use axum::{Router, body::Bytes, extract::{Path, State}, http::StatusCode, routing::get};

use crate::contexts::banking::public::BankingFacade;

pub(crate) fn webhook_router(banking: BankingFacade) -> Router {
    Router::new().route("/webhooks/monobank/{webhook_credential}",get(validate).post(receive)).with_state(banking)
}

async fn validate(State(banking):State<BankingFacade>,Path(credential):Path<String>)->StatusCode{match banking.validate_webhook_credential(&credential).await{Ok(true)=>StatusCode::OK,_=>StatusCode::NOT_FOUND}}
async fn receive(State(banking):State<BankingFacade>,Path(credential):Path<String>,body:Bytes)->StatusCode{match banking.receive_webhook(&credential,&body).await{Ok(_)=>StatusCode::OK,_=>StatusCode::NOT_FOUND}}
