use std::{env, fs::{File, create_dir_all, remove_dir_all}, io::copy, path::{Path, PathBuf}};
use anyhow::{Result, bail, anyhow};
use flate2::read::GzDecoder;
use tar::Archive;
use serde::Deserialize;
use std::process::Command;

const WASI_SDK_URL: &str = "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-25/";
const WASI_SDK_ASSET_NAME: Option<&str> = cfg_select! {
    all(target_os = "macos", target_arch = "x86_64") => Some("wasi-sdk-25.0-x86_64-macos.tar.gz"),
    all(target_os = "macos", target_arch = "aarch64") => Some("wasi-sdk-25.0-arm64-macos.tar.gz"),
    all(target_os = "linux", target_arch = "x86_64") => Some("wasi-sdk-25.0-x86_64-linux.tar.gz"),
    all(target_os = "linux", target_arch = "aarch64") => Some("wasi-sdk-25.0-arm64-linux.tar.gz"),
    all(target_os = "freebsd", target_arch = "x86_64") => Some("wasi-sdk-25.0-x86_64-linux.tar.gz"),
    all(target_os = "freebsd", target_arch = "aarch64") => Some("wasi-sdk-25.0-arm64-linux.tar.gz"),
    all(target_os = "windows", target_arch = "x86_64") => Some("wasi-sdk-25.0-x86_64-windows.tar.gz"),
    _ => None
};

fn tool_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn cache_dir() -> PathBuf {
    tool_dir().join(".cache")
}

fn download(url: &str, output_path: &Path) -> Result<()> {
    let response = ureq::get(url).call()?;
    let mut reader = response.into_reader();
    let mut file = File::create(output_path)?;
    copy(&mut reader, &mut file)?;
    Ok(())
}

fn untar_gz(tar_path: &Path, output_dir: &Path) -> Result<()> {
    let tar_gz = File::open(tar_path)?;
    let decoder = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(decoder);
    archive.unpack(output_dir)?;
    Ok(())
}

fn setup() -> Result<()> {
    match WASI_SDK_ASSET_NAME {
        Some(wasi_tar_filename) => {
            let wasi_dir = cache_dir().join("wasi");
            let wasi_tar_dir = wasi_dir.join("tar");
            create_dir_all(&wasi_tar_dir)?;

            let wasi_tar = wasi_tar_dir.join(wasi_tar_filename);
            if wasi_tar.exists() {
                println!("found WASI: {}", wasi_tar.as_path().display());
            } else {
                let out = wasi_tar.as_path();
                println!("downloading WASI: {}", out.display());
                download(&format!("{WASI_SDK_URL}{wasi_tar_filename}"), &out)?;
            }

            // always extract fresh copy
            {
                let wasi_install_dir = wasi_dir.join("install");
                remove_dir_all(&wasi_install_dir).ok();
                create_dir_all(&wasi_install_dir)?;
                let out = wasi_install_dir.as_path();
                println!("extracting WASI: {}", out.display());
                untar_gz(wasi_tar.as_path(), out)?;
            }

            Ok(())
        },
        _ => bail!("Unknown target os/arch")
    }
}

#[derive(Debug)]
struct BuildArgs {
    input: PathBuf,
    output: PathBuf,
}

fn parse_build_args(mut args: impl Iterator<Item = String>) -> Result<BuildArgs> {
    let mut input = None;
    let mut output = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-i" | "--input" => {
                input = args.next();
            }
            "-o" | "--output" => {
                output = args.next();
            }
            _ => bail!("unknown argument: {arg}"),
        }
    }

    Ok(BuildArgs {
        input: PathBuf::from(input.ok_or_else(|| anyhow!("missing -i <input.toml>"))?),
        output: PathBuf::from(output.ok_or_else(|| anyhow!("missing -o <output/dir>"))?)
    })
}

#[derive(Debug, Deserialize)]
struct GrammarManifest {
    grammar: Vec<Grammar>,
}

#[derive(Debug, Deserialize)]
struct Grammar {
    id: String,
    repository: String,
    commit: String,
}

fn read_manifest(path: &Path) -> Result<GrammarManifest> {
    let text = std::fs::read_to_string(path)?;
    let manifest: GrammarManifest = toml::from_str(&text)?;
    Ok(manifest)
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        bail!("command failed: {command:?}");
    }
    Ok(())
}

fn build(clang_path: PathBuf, args: &BuildArgs) -> Result<()> {
    if !clang_path.exists() {
        bail!("WASI SDK missing; please run:\n  grammar-builder setup");
    }

    let manifest = read_manifest(args.input.as_path())?;
    let grammar_root_dir = cache_dir().join("grammar");
    create_dir_all(&grammar_root_dir)?;

    for grammar in manifest.grammar {
        let grammar_dir = grammar_root_dir.join(&grammar.id);

        if !grammar_dir.exists() {
            println!("cloning {}", grammar.repository);
            run(
                Command::new("git")
                    .arg("clone")
                    .arg("--depth")
                    .arg("1")
                    .arg("--revision")
                    .arg(&grammar.commit)
                    .arg(&grammar.repository)
                    .arg(&grammar_dir)
            )?;
        }

        let output_dir = &args.output;
        let grammar_wasm = output_dir.join(format!("{}.wasm", grammar.id));
        if !grammar_wasm.exists() {
            create_dir_all(&output_dir)?;
            let src_dir = grammar_dir.join("src");
            let parser_c = src_dir.join("parser.c");
            let scanner_c = src_dir.join("scanner.c");
            println!("compiling {}", grammar.id);
            let mut cmd = Command::new(&clang_path);
            cmd
                .args(["-fPIC", "-shared", "-Os"])
                .arg(format!("-Wl,--export=tree_sitter_{}", grammar.id))
                .arg("-o")
                .arg(&grammar_wasm)
                .arg("-I")
                .arg(&src_dir)
                .arg(&parser_c);
            if scanner_c.exists() {
                cmd.arg(&scanner_c);
            }
            run(&mut cmd)?;
        } else {
            println!("skipping {}; output already exists", grammar.id);
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    let command = env::args().nth(1).unwrap_or_else(|| "help".to_string());

    match command.as_str() {
        "setup" => setup(),
        "build" => {
            let wasi_sdk_dir = cache_dir().join("wasi").join("install")
                .join(WASI_SDK_ASSET_NAME.unwrap().trim_end_matches(".tar.gz"));
            let clang_path = wasi_sdk_dir.join("bin").join(&format!("clang{}", env::consts::EXE_SUFFIX));
            build(clang_path, &parse_build_args(env::args().skip(2))?)
        },
        _ => {
            println!(
r#"Usage:
  grammar-builder <command>

Commands:
  setup                                  - initialize local cache
  build -i <input.toml> -o <output/dir>  - build grammars specified in input.toml"#);
            Ok(())
        }
    }
}
