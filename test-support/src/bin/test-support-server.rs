use anyhow::Result;
use std::path::PathBuf;

fn main() -> Result<()> {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();

    let mut workspace_path = None;
    let mut project_type = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--workspace" => {
                if i + 1 < args.len() {
                    workspace_path = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                } else {
                    eprintln!("Missing workspace path after --workspace");
                    std::process::exit(1);
                }
            }
            "--project-type" => {
                if i + 1 < args.len() {
                    project_type = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Missing project type after --project-type");
                    std::process::exit(1);
                }
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                std::process::exit(1);
            }
        }
    }

    let workspace_path = workspace_path.expect("--workspace is required");
    let project_type = project_type.expect("--project-type is required");

    // Start the IPC server (blocking call)
    test_support::ipc::start_server(&workspace_path, &project_type)?;

    Ok(())
}
