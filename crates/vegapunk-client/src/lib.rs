pub mod client;

#[allow(clippy::doc_overindented_list_items)]
#[allow(clippy::result_large_err)]
pub mod proto {
    tonic::include_proto!("graphrag");
}
