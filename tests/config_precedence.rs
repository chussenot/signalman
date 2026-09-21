//! The configuration layers, exercised through the binary so the environment
//! layer is real: `config show` prints the effective TOML after defaults,
//! file, environment and flags.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::Command;

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_signalman"));
    // Start from a clean slate: none of the variables the module reads.
    for (var, _) in signalman::config::ENV_VARS {
        c.env_remove(var);
    }
    c.env_remove(signalman::config::FILE_ENV);
    c
}

fn write(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("signalman-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

fn effective(cmd: &mut Command) -> toml::Value {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.starts_with("# signalman effective configuration"),
        "{text}"
    );
    toml::from_str(&text).unwrap()
}

#[test]
fn defaults_then_file_then_env_then_flag() {
    let file = write(
        "layers.toml",
        r#"
        [server]
        addr = "0.0.0.0:9100"
        [flow]
        related_window_minutes = 15
        note = false
        [policy]
        page_at = "outage"
        "#,
    );

    // Defaults only.
    let v = effective(
        bin()
            .args(["config", "show"])
            .current_dir(std::env::temp_dir()),
    );
    assert_eq!(v["server"]["addr"].as_str(), Some("127.0.0.1:8080"));
    assert_eq!(v["flow"]["related_window_minutes"].as_integer(), Some(30));
    assert_eq!(v["flow"]["note"].as_bool(), Some(true));
    assert_eq!(v["policy"]["page_at"].as_str(), Some("major"));
    assert_eq!(v["triage"]["teams"].as_array().unwrap().len(), 6);

    // File overrides defaults.
    let v = effective(bin().args(["--config", file.to_str().unwrap(), "config", "show"]));
    assert_eq!(v["server"]["addr"].as_str(), Some("0.0.0.0:9100"));
    assert_eq!(v["flow"]["related_window_minutes"].as_integer(), Some(15));
    assert_eq!(v["flow"]["note"].as_bool(), Some(false));
    assert_eq!(v["policy"]["page_at"].as_str(), Some("outage"));

    // Environment overrides the file; the file can also be named by env.
    let v = effective(
        bin()
            .env(signalman::config::FILE_ENV, &file)
            .env("SIGNALMAN_RELATED_WINDOW_MINUTES", "5")
            .env("SIGNALMAN_NOTE", "true")
            .env("SIGNALMAN_ADDR", "") // empty counts as unset
            .args(["config", "show"]),
    );
    assert_eq!(v["server"]["addr"].as_str(), Some("0.0.0.0:9100"));
    assert_eq!(v["flow"]["related_window_minutes"].as_integer(), Some(5));
    assert_eq!(v["flow"]["note"].as_bool(), Some(true));

    // A flag overrides the environment. `serve` is the command carrying the
    // flags; `config show` resolves with the same function, so assert through
    // the library on the CLI layer instead of starting a server.
    let settings =
        signalman::config::Settings::parse(&std::fs::read_to_string(&file).unwrap(), &file)
            .unwrap();
    let cli = signalman::config::Overrides {
        related_window_minutes: Some(1),
        note: Some(false),
        ..Default::default()
    };
    let cfg = signalman::config::Config::resolve(&settings, &cli).unwrap();
    assert_eq!(cfg.flow.related_window_minutes, 1);
    assert!(!cfg.flow.note);
}

#[test]
fn invalid_file_and_env_values_fail_with_the_offending_name() {
    let file = write("bad.toml", "[policy]\nsupress_below = 0.1\n");
    let out = bin()
        .args(["--config", file.to_str().unwrap(), "config", "show"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("supress_below"), "{err}");

    let out = bin()
        .env("SIGNALMAN_RELATED_WINDOW_MINUTES", "soon")
        .args(["config", "show"])
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("SIGNALMAN_RELATED_WINDOW_MINUTES")
            && err.contains("flow.related_window_minutes"),
        "{err}"
    );

    let missing = std::env::temp_dir().join("signalman-does-not-exist.toml");
    let out = bin()
        .args(["--config", missing.to_str().unwrap(), "config", "show"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn secrets_are_refused_from_the_file_and_never_printed() {
    let file = write("secret.toml", "[typesafe]\napi_key = \"sk-live\"\n");
    let out = bin()
        .args(["--config", file.to_str().unwrap(), "config", "show"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("api_key"));

    let out = bin()
        .env("TYPESAFE_API_KEY", "sk-live-secret")
        .args(["config", "show"])
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("sk-live-secret"));
}

#[test]
fn every_documented_env_var_appears_in_the_configuration_page() {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/configuration.md"
    ))
    .unwrap();
    for (var, key) in signalman::config::ENV_VARS {
        assert!(doc.contains(var), "docs/configuration.md lacks {var}");
        assert!(doc.contains(key), "docs/configuration.md lacks {key}");
    }
}
