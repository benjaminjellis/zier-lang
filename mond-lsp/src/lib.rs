mod analysis;
mod backend;
pub(crate) mod project;
mod semantic_tokens;
pub(crate) mod state;
#[cfg(test)]
mod tests;

use analysis::*;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use codespan_reporting::diagnostic::{Diagnostic as CodeDiagnostic, LabelStyle, Severity};
use tokio::io::{AsyncRead, AsyncWrite};
use tower_lsp::{
    LspService, Server,
    lsp_types::{Diagnostic, SymbolKind, Url},
};

use crate::{backend::Backend, state::ServerState};

pub async fn serve<R, W>(stdin: R, stdout: W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
