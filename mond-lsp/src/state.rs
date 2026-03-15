use std::collections::HashMap;

use tower_lsp::lsp_types::Url;

#[derive(Clone, Debug)]
pub(crate) struct DocumentState {
    pub(crate) version: i32,
    pub(crate) text: String,
}

#[derive(Default)]
pub(crate) struct ServerState {
    pub(crate) open_docs: HashMap<Url, DocumentState>,
}
