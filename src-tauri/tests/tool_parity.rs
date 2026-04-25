use async_trait::async_trait;
use ftp_client_gui_lib::ai_core::{
    credential_provider::{
        CredentialProvider, ProviderExtraOptions, ServerCredentials, ServerProfile,
    },
    event_sink::{EventSink, ToolProgress},
    remote_backend::{RemoteBackend, StorageQuota},
    tools::{dispatch_tool, Surfaces, ToolCtx},
};
use std::sync::Arc;
use tokio::sync::Mutex;

struct TestSink;
impl EventSink for TestSink {
    fn emit_stream_chunk(&self, _: &str, _: &ftp_client_gui_lib::ai_stream::StreamChunk) {}
    fn emit_tool_progress(&self, _: &ToolProgress) {}
    fn emit_app_control(&self, _: &str, _: &serde_json::Value) {}
}

struct TestCreds;
impl CredentialProvider for TestCreds {
    fn list_servers(&self) -> Result<Vec<ServerProfile>, String> {
        Ok(vec![])
    }
    fn get_credentials(&self, _: &str) -> Result<ServerCredentials, String> {
        Err("test".into())
    }
    fn get_extra_options(&self, _: &str) -> Result<ProviderExtraOptions, String> {
        Ok(ProviderExtraOptions::new())
    }
}

struct TestCtx {
    surface: Surfaces,
    sink: TestSink,
    creds: TestCreds,
    backend: Option<Arc<dyn RemoteBackend>>,
}

#[async_trait]
impl ToolCtx for TestCtx {
    fn event_sink(&self) -> &dyn EventSink {
        &self.sink
    }
    fn credentials(&self) -> &dyn CredentialProvider {
        &self.creds
    }
    async fn remote_backend(&self, _: &str) -> Result<Arc<dyn RemoteBackend>, String> {
        self.backend.clone().ok_or_else(|| "test".into())
    }
    fn surface(&self) -> Surfaces {
        self.surface
    }
}

fn gui_ctx() -> TestCtx {
    TestCtx {
        surface: Surfaces::GUI,
        sink: TestSink,
        creds: TestCreds,
        backend: None,
    }
}

fn cli_ctx() -> TestCtx {
    TestCtx {
        surface: Surfaces::CLI,
        sink: TestSink,
        creds: TestCreds,
        backend: None,
    }
}

fn remote_ctx(surface: Surfaces, backend: Arc<dyn RemoteBackend>) -> TestCtx {
    TestCtx {
        surface,
        sink: TestSink,
        creds: TestCreds,
        backend: Some(backend),
    }
}

#[derive(Default)]
struct FakeRemoteBackend {
    uploads: Mutex<Vec<(String, Vec<u8>)>>,
}

fn fake_entry(
    name: &str,
    path: &str,
    is_dir: bool,
    size: u64,
) -> ftp_client_gui_lib::providers::RemoteEntry {
    ftp_client_gui_lib::providers::RemoteEntry {
        name: name.to_string(),
        path: path.to_string(),
        is_dir,
        size,
        modified: Some("2026-04-25T00:00:00Z".to_string()),
        permissions: Some("rw-r--r--".to_string()),
        owner: Some("owner".to_string()),
        group: None,
        is_symlink: false,
        link_target: None,
        mime_type: None,
        metadata: Default::default(),
    }
}

#[async_trait]
impl RemoteBackend for FakeRemoteBackend {
    async fn is_connected(&self) -> bool {
        true
    }

    async fn list(
        &self,
        path: &str,
    ) -> Result<Vec<ftp_client_gui_lib::providers::RemoteEntry>, String> {
        Ok(vec![
            fake_entry(
                "hello.txt",
                &format!("{}/hello.txt", path.trim_end_matches('/')),
                false,
                11,
            ),
            fake_entry(
                "docs",
                &format!("{}/docs", path.trim_end_matches('/')),
                true,
                0,
            ),
        ])
    }

    async fn stat(&self, path: &str) -> Result<ftp_client_gui_lib::providers::RemoteEntry, String> {
        Ok(fake_entry("hello.txt", path, false, 11))
    }

    async fn download_to_bytes(&self, _: &str) -> Result<Vec<u8>, String> {
        Ok(b"hello world".to_vec())
    }

    async fn upload_from_bytes(&self, data: &[u8], path: &str) -> Result<(), String> {
        self.uploads
            .lock()
            .await
            .push((path.to_string(), data.to_vec()));
        Ok(())
    }

    async fn download(&self, _: &str, local: &str) -> Result<(), String> {
        std::fs::write(local, b"downloaded").map_err(|e| e.to_string())
    }

    async fn upload(&self, local: &str, remote: &str) -> Result<(), String> {
        let bytes = std::fs::read(local).map_err(|e| e.to_string())?;
        self.uploads.lock().await.push((remote.to_string(), bytes));
        Ok(())
    }

    async fn delete(&self, _: &str) -> Result<(), String> {
        Ok(())
    }

    async fn mkdir(&self, _: &str) -> Result<(), String> {
        Ok(())
    }

    async fn rename(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }

    async fn search(
        &self,
        path: &str,
        pattern: &str,
    ) -> Result<Vec<ftp_client_gui_lib::providers::RemoteEntry>, String> {
        Ok(vec![fake_entry(
            pattern,
            &format!("{}/{}", path.trim_end_matches('/'), pattern),
            false,
            3,
        )])
    }

    async fn storage_info(&self) -> Result<StorageQuota, String> {
        Ok(StorageQuota {
            used: 10,
            total: 100,
            available: 90,
        })
    }
}

#[tokio::test]
async fn local_read_parity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("f.txt");
    std::fs::write(&path, "hello world").unwrap();
    let args = serde_json::json!({ "path": path.to_str().unwrap() });

    let r1 = dispatch_tool(&gui_ctx(), "local_read", &args)
        .await
        .unwrap();
    let r2 = dispatch_tool(&cli_ctx(), "local_read", &args)
        .await
        .unwrap();

    assert_eq!(r1["content"], r2["content"]);
    assert_eq!(r1["size"], r2["size"]);
    assert_eq!(r1["truncated"], r2["truncated"]);
}

#[tokio::test]
async fn local_write_parity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path1 = tmp.path().join("gui.txt");
    let path2 = tmp.path().join("cli.txt");

    let args1 = serde_json::json!({ "path": path1.to_str().unwrap(), "content": "gui" });
    let args2 = serde_json::json!({ "path": path2.to_str().unwrap(), "content": "cli" });

    let r1 = dispatch_tool(&gui_ctx(), "local_write", &args1)
        .await
        .unwrap();
    let r2 = dispatch_tool(&cli_ctx(), "local_write", &args2)
        .await
        .unwrap();

    assert_eq!(r1["success"], r2["success"]);
    assert!(r1["success"].as_bool().unwrap());

    let c1 = std::fs::read_to_string(&path1).unwrap();
    let c2 = std::fs::read_to_string(&path2).unwrap();
    assert_eq!(c1, "gui");
    assert_eq!(c2, "cli");
}

#[tokio::test]
async fn local_mkdir_parity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir1 = tmp.path().join("gui_dir");
    let dir2 = tmp.path().join("cli_dir");

    let args1 = serde_json::json!({ "path": dir1.to_str().unwrap() });
    let args2 = serde_json::json!({ "path": dir2.to_str().unwrap() });

    let r1 = dispatch_tool(&gui_ctx(), "local_mkdir", &args1)
        .await
        .unwrap();
    let r2 = dispatch_tool(&cli_ctx(), "local_mkdir", &args2)
        .await
        .unwrap();

    assert_eq!(r1["success"], r2["success"]);
    assert!(r1["success"].as_bool().unwrap());

    assert!(dir1.is_dir());
    assert!(dir2.is_dir());
}

#[tokio::test]
async fn local_delete_parity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path1 = tmp.path().join("del1.txt");
    let path2 = tmp.path().join("del2.txt");
    std::fs::write(&path1, "test1").unwrap();
    std::fs::write(&path2, "test2").unwrap();

    let args1 = serde_json::json!({ "path": path1.to_str().unwrap() });
    let args2 = serde_json::json!({ "path": path2.to_str().unwrap() });

    let r1 = dispatch_tool(&gui_ctx(), "local_delete", &args1)
        .await
        .unwrap();
    let r2 = dispatch_tool(&cli_ctx(), "local_delete", &args2)
        .await
        .unwrap();

    assert_eq!(r1["success"], r2["success"]);
    assert!(r1["success"].as_bool().unwrap());

    assert!(!path1.exists());
    assert!(!path2.exists());
}

#[tokio::test]
async fn local_grep_parity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let f1 = tmp.path().join("f1.txt");
    let f2 = tmp.path().join("f2.txt");
    std::fs::write(&f1, "hello world\nfoo bar\n").unwrap();
    std::fs::write(&f2, "baz qux\nhello rust\n").unwrap();

    let args = serde_json::json!({
        "path": tmp.path().to_str().unwrap(),
        "pattern": "hello"
    });

    let r1 = dispatch_tool(&gui_ctx(), "local_grep", &args)
        .await
        .unwrap();
    let r2 = dispatch_tool(&cli_ctx(), "local_grep", &args)
        .await
        .unwrap();

    assert_eq!(r1["total_matches"], r2["total_matches"]);
    assert_eq!(r1["total_matches"].as_u64().unwrap(), 2);
    // GUI and CLI should return structurally identical outputs.
    assert_eq!(
        r1["matches"].as_array().unwrap().len(),
        r2["matches"].as_array().unwrap().len()
    );
}

#[tokio::test]
async fn shell_execute_parity() {
    let args = serde_json::json!({
        "command": "echo 'hello test'"
    });

    let r1 = dispatch_tool(&gui_ctx(), "shell_execute", &args)
        .await
        .unwrap();
    let r2 = dispatch_tool(&cli_ctx(), "shell_execute", &args)
        .await
        .unwrap();

    assert_eq!(r1["success"], r2["success"]);
    assert!(r1["success"].as_bool().unwrap());

    // Output should contain the echoed text
    let out1 = r1["stdout"].as_str().unwrap();
    let out2 = r2["stdout"].as_str().unwrap();
    assert!(out1.contains("hello test"));
    assert!(out2.contains("hello test"));
}

#[tokio::test]
async fn remote_list_alias_matches_aeroftp_list_files() {
    let backend: Arc<dyn RemoteBackend> = Arc::new(FakeRemoteBackend::default());
    let ctx = remote_ctx(Surfaces::MCP, backend);
    let args = serde_json::json!({ "server": "Test", "path": "/root" });

    let r1 = dispatch_tool(&ctx, "aeroftp_list_files", &args)
        .await
        .unwrap();
    let r2 = dispatch_tool(&ctx, "remote_list", &args).await.unwrap();

    assert_eq!(r1["entries"], r2["entries"]);
    assert_eq!(r1["total"], r2["total"]);
}

#[tokio::test]
async fn remote_read_alias_matches_aeroftp_read_file() {
    let backend: Arc<dyn RemoteBackend> = Arc::new(FakeRemoteBackend::default());
    let ctx = remote_ctx(Surfaces::MCP, backend);
    let args = serde_json::json!({ "server": "Test", "path": "/hello.txt" });

    let r1 = dispatch_tool(&ctx, "aeroftp_read_file", &args)
        .await
        .unwrap();
    let r2 = dispatch_tool(&ctx, "remote_read", &args).await.unwrap();

    assert_eq!(r1["content"], r2["content"]);
    assert_eq!(r1["size"], r2["size"]);
    assert_eq!(r1["truncated"], r2["truncated"]);
}

#[tokio::test]
async fn rag_index_parity() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "fn main() {}\n").unwrap();
    std::fs::write(tmp.path().join("b.md"), "# Hello\n\nworld\n").unwrap();
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("c.txt"), "deep file\n").unwrap();

    let args = serde_json::json!({
        "path": tmp.path().to_str().unwrap(),
        "recursive": true,
        "max_files": 100,
    });

    let r1 = dispatch_tool(&gui_ctx(), "rag_index", &args).await.unwrap();
    let r2 = dispatch_tool(&cli_ctx(), "rag_index", &args).await.unwrap();

    assert_eq!(r1["files_count"], r2["files_count"]);
    assert_eq!(r1["dirs_count"], r2["dirs_count"]);
    assert_eq!(r1["total_size"], r2["total_size"]);
    assert_eq!(r1["files_count"].as_u64().unwrap(), 3);
}

#[tokio::test]
async fn rag_search_parity() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("alpha.txt"), "hello world\nfoo bar\n").unwrap();
    std::fs::write(tmp.path().join("beta.txt"), "no match here\n").unwrap();

    let args = serde_json::json!({
        "query": "hello",
        "path": tmp.path().to_str().unwrap(),
        "max_results": 50,
    });

    let r1 = dispatch_tool(&gui_ctx(), "rag_search", &args)
        .await
        .unwrap();
    let r2 = dispatch_tool(&cli_ctx(), "rag_search", &args)
        .await
        .unwrap();

    assert_eq!(r1["matches"].as_array().unwrap().len(), 1);
    assert_eq!(
        r1["matches"].as_array().unwrap().len(),
        r2["matches"].as_array().unwrap().len()
    );
    assert_eq!(r1["query"], r2["query"]);
}

#[tokio::test]
async fn remote_upload_content_works_through_core_backend() {
    let backend = Arc::new(FakeRemoteBackend::default());
    let ctx = remote_ctx(Surfaces::MCP, backend.clone());
    let args = serde_json::json!({
        "server": "Test",
        "remote_path": "/out.txt",
        "content": "payload"
    });

    let result = dispatch_tool(&ctx, "aeroftp_upload_file", &args)
        .await
        .unwrap();

    assert_eq!(result["uploaded"], true);
    let uploads = backend.uploads.lock().await;
    assert_eq!(uploads[0].0, "/out.txt");
    assert_eq!(uploads[0].1, b"payload");
}
