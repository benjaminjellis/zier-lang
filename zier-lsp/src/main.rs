#[tokio::main]
async fn main() {
    zier_lsp::serve(tokio::io::stdin(), tokio::io::stdout()).await;
}
