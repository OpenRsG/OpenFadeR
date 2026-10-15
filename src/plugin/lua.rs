//! Lua plugins (`"script": "main.lua"`): the script runs inside the app, in a fresh Lua 5.4 state per request (as a
//! process plugin is a fresh process per request), on the thread that asked. It answers through a function
//! `handle(request)` (global, or a field of the table the script returns) that gets the request as a Lua table and returns
//! the reply table; `error(...)` or a reply with an `error` field is a failed request. The standard library is the safe
//! one (no `debug`, no C modules); `require` finds `*.lua` files of the plugin folder. A script running past the request's
//! timeout is stopped.
//!
//! The `rk` table gives what a plugin needs beyond plain Lua:
//! - `rk.name`, `rk.dir` (plugin folder), `rk.data_dir` (`<data dir>/plugins/<name>`, created on first use), `rk.os`;
//! - `rk.log(...)` (stderr, prefixed with the plugin's name; `print` does the same);
//! - `rk.json.encode(value)`, `rk.json.decode(text)`;
//! - `rk.http.get(url [, headers])` and `rk.http.post(url, body [, headers])` → `body, status`;
//!   `rk.http.download(url, path)` → `status` (the body written to `path` as is: audio for a source's `fetch`);
//! - `rk.run(program, {args…})` → `stdout, exit code` (stdout only; the program's stderr is passed on);
//! - `rk.fs.read(path)` (`nil` when missing), `rk.fs.write(path, text)`, `rk.fs.exists(path)`, `rk.fs.list(dir)`,
//!   `rk.fs.mkdir(path)`; relative paths are in the plugin folder;
//! - `rk.sleep(ms)`.

use super::Plugin;
use mlua::{
    HookTriggers, Lua, LuaOptions, LuaSerdeExt, MultiValue, StdLib, Table, Value as LuaValue,
    VmState,
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Runs `script` of `plugin` on `request` (see the module doc).
pub(super) fn call(
    plugin: &Plugin,
    script: &str,
    request: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    let name = &plugin.manifest.name;
    run(plugin, script, request, timeout).map_err(|e| format!("{name}: {e}"))
}

fn run(plugin: &Plugin, script: &str, request: &Value, timeout: Duration) -> Result<Value, String> {
    let file = plugin.dir.join(script);
    let source = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    let secs = timeout.as_secs();
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(10_000),
        move |_, _| {
            if Instant::now() < deadline {
                Ok(VmState::Continue)
            } else {
                Err(mlua::Error::runtime(format!("no reply within {secs} s")))
            }
        },
    )
    .map_err(|e| e.to_string())?;
    api(&lua, plugin).map_err(|e| e.to_string())?;
    let module: LuaValue = lua
        .load(&source)
        .set_name(format!("@{}", file.display()))
        .eval()
        .map_err(|e| e.to_string())?;
    let handle = match &module {
        LuaValue::Table(t) => t
            .get::<Option<mlua::Function>>("handle")
            .map_err(|e| e.to_string())?,
        _ => None,
    };
    let handle = match handle {
        Some(h) => h,
        None => lua
            .globals()
            .get::<Option<mlua::Function>>("handle")
            .map_err(|e| e.to_string())?
            .ok_or("the script defines no `handle(request)` function")?,
    };
    let reply: LuaValue = handle
        .call(lua.to_value(request).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let reply: Value = match reply {
        LuaValue::Nil => Value::Object(Default::default()),
        v => lua.from_value(v).map_err(|e| format!("reply: {e}"))?,
    };
    // An empty Lua table converts to an empty list; a reply is an object.
    let reply = match reply {
        Value::Array(a) if a.is_empty() => Value::Object(Default::default()),
        r => r,
    };
    if let Some(e) = reply.get("error").and_then(Value::as_str) {
        return Err(e.to_owned());
    }
    Ok(reply)
}

/// A path the script gave: relative ones are in the plugin folder.
fn at(dir: &Path, p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_relative() {
        dir.join(p)
    } else {
        p.to_owned()
    }
}

/// HTTP for `rk.http`: status codes come back to the script instead of failing the call.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into()
}

/// `{name = value}` header tables to pairs.
fn headers(t: Option<Table>) -> mlua::Result<Vec<(String, String)>> {
    t.map_or(Ok(Vec::new()), |t| t.pairs::<String, String>().collect())
}

/// Installs `rk`, `print` and `package.path` (see the module doc).
fn api(lua: &Lua, plugin: &Plugin) -> mlua::Result<()> {
    let (name, dir) = (plugin.manifest.name.clone(), plugin.dir.clone());
    let rk = lua.create_table()?;
    rk.set("name", name.as_str())?;
    rk.set("dir", dir.to_string_lossy().as_ref())?;
    let data = crate::paths::data_dir().map(|d| d.join("plugins").join(&name));
    rk.set(
        "data_dir",
        data.as_ref().map(|d| d.to_string_lossy().into_owned()),
    )?;
    if let Some(d) = &data {
        let _ = std::fs::create_dir_all(d);
    }
    rk.set("os", std::env::consts::OS)?;

    let who = name.clone();
    let log = lua.create_function(move |_, args: MultiValue| {
        let parts: Vec<String> = args
            .iter()
            .map(|v| v.to_string().unwrap_or_else(|_| format!("{v:?}")))
            .collect();
        eprintln!("plugin {who}: {}", parts.join(" "));
        Ok(())
    })?;
    rk.set("log", log.clone())?;
    lua.globals().set("print", log)?;

    let json = lua.create_table()?;
    json.set(
        "encode",
        lua.create_function(|lua, v: LuaValue| {
            let v: Value = lua.from_value(v)?;
            Ok(v.to_string())
        })?,
    )?;
    json.set(
        "decode",
        lua.create_function(|lua, s: String| {
            let v: Value = serde_json::from_str(&s).map_err(mlua::Error::external)?;
            lua.to_value(&v)
        })?,
    )?;
    rk.set("json", json)?;

    let http = lua.create_table()?;
    http.set(
        "get",
        lua.create_function(|_, (url, h): (String, Option<Table>)| {
            let mut req = agent().get(&url);
            for (k, v) in headers(h)? {
                req = req.header(k, v);
            }
            let mut resp = req.call().map_err(mlua::Error::external)?;
            let status = resp.status().as_u16();
            Ok((
                resp.body_mut()
                    .read_to_string()
                    .map_err(mlua::Error::external)?,
                status,
            ))
        })?,
    )?;
    http.set(
        "post",
        lua.create_function(|_, (url, body, h): (String, String, Option<Table>)| {
            let mut req = agent().post(&url);
            for (k, v) in headers(h)? {
                req = req.header(k, v);
            }
            let mut resp = req.send(body).map_err(mlua::Error::external)?;
            let status = resp.status().as_u16();
            Ok((
                resp.body_mut()
                    .read_to_string()
                    .map_err(mlua::Error::external)?,
                status,
            ))
        })?,
    )?;
    let d = dir.clone();
    http.set(
        "download",
        lua.create_function(move |_, (url, path): (String, String)| {
            let mut resp = agent().get(&url).call().map_err(mlua::Error::external)?;
            let status = resp.status().as_u16();
            let dest = at(&d, &path);
            let mut out = std::fs::File::create(&dest).map_err(mlua::Error::external)?;
            std::io::copy(&mut resp.body_mut().as_reader(), &mut out)
                .map_err(mlua::Error::external)?;
            Ok(status)
        })?,
    )?;
    rk.set("http", http)?;

    let d = dir.clone();
    rk.set(
        "run",
        lua.create_function(move |_, (program, args): (String, Option<Vec<String>>)| {
            let out = std::process::Command::new(&program)
                .args(args.unwrap_or_default())
                .current_dir(&d)
                .stderr(std::process::Stdio::inherit())
                .output()
                .map_err(|e| mlua::Error::runtime(format!("cannot start {program:?}: {e}")))?;
            Ok((
                String::from_utf8_lossy(&out.stdout).into_owned(),
                out.status.code().unwrap_or(-1),
            ))
        })?,
    )?;

    let fs = lua.create_table()?;
    let d = dir.clone();
    fs.set(
        "read",
        lua.create_function(move |_, p: String| Ok(std::fs::read_to_string(at(&d, &p)).ok()))?,
    )?;
    let d = dir.clone();
    fs.set(
        "write",
        lua.create_function(move |_, (p, text): (String, String)| {
            std::fs::write(at(&d, &p), text).map_err(mlua::Error::external)
        })?,
    )?;
    let d = dir.clone();
    fs.set(
        "exists",
        lua.create_function(move |_, p: String| Ok(at(&d, &p).exists()))?,
    )?;
    let d = dir.clone();
    fs.set(
        "list",
        lua.create_function(move |_, p: String| {
            let mut names: Vec<String> = std::fs::read_dir(at(&d, &p))
                .map_err(mlua::Error::external)?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            Ok(names)
        })?,
    )?;
    let d = dir.clone();
    fs.set(
        "mkdir",
        lua.create_function(move |_, p: String| {
            std::fs::create_dir_all(at(&d, &p)).map_err(mlua::Error::external)
        })?,
    )?;
    rk.set("fs", fs)?;
    rk.set(
        "sleep",
        lua.create_function(|_, ms: u64| {
            std::thread::sleep(Duration::from_millis(ms.min(60_000)));
            Ok(())
        })?,
    )?;
    lua.globals().set("rk", rk)?;

    let package: Table = lua.globals().get("package")?;
    let d = dir.to_string_lossy();
    package.set("path", format!("{d}/?.lua;{d}/?/init.lua"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::Manifest;
    use serde_json::json;

    fn plugin(script: &str) -> (Plugin, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("rk-lua-{}-{}", std::process::id(), script.len()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.lua"), script).unwrap();
        std::fs::write(
            dir.join("helper.lua"),
            "return {twice = function(x) return 2 * x end}",
        )
        .unwrap();
        let manifest = Manifest {
            name: "lua-test".into(),
            script: Some("main.lua".into()),
            ..Manifest::default()
        };
        (
            Plugin {
                dir: dir.clone(),
                manifest,
            },
            dir,
        )
    }

    #[test]
    fn a_script_answers_through_handle_with_json_require_and_files() {
        let (p, dir) = plugin(
            r#"
            local h = require("helper")
            local M = {}
            function M.handle(req)
              rk.fs.write("out.txt", req.op)
              local back = rk.json.decode(rk.json.encode({n = h.twice(req.n)}))
              return {op = req.op, n = back.n, read = rk.fs.read("out.txt"), missing = rk.fs.read("nope") == nil,
                      name = rk.name, tracks = {{title = "A"}}}
            end
            return M
            "#,
        );
        let reply = p
            .call(&json!({"op": "search", "n": 21}), Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            reply,
            json!({"op": "search", "n": 42, "read": "search", "missing": true, "name": "lua-test", "tracks": [{"title": "A"}]})
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn errors_and_runaway_scripts_fail_the_request() {
        let (p, dir) = plugin(
            "function handle(req) if req.op == 'boom' then error('bad op') end while true do end end",
        );
        let e = p
            .call(&json!({"op": "boom"}), Duration::from_secs(5))
            .unwrap_err();
        assert!(e.starts_with("lua-test: ") && e.contains("bad op"), "{e}");
        let t = Instant::now();
        let e = p
            .call(&json!({"op": "spin"}), Duration::from_millis(200))
            .unwrap_err();
        assert!(e.contains("no reply within"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(2));
        let (p, dir2) = plugin("function handle(req) return {error = 'nope ' .. req.op} end");
        assert_eq!(
            p.call(&json!({"op": "x"}), Duration::from_secs(5))
                .unwrap_err(),
            "lua-test: nope x"
        );
        std::fs::remove_dir_all(dir).ok();
        std::fs::remove_dir_all(dir2).ok();
    }
}
