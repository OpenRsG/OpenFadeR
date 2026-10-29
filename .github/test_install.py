#!/usr/bin/env python3
"""Exercise the Linux installer with tiny loadable runtime/plugin fixtures, without a Rust build."""
import ctypes
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent


@unittest.skipUnless(sys.platform.startswith("linux"), "Linux installer")
class InstallTests(unittest.TestCase):
    def check_install(self, with_runtimes):
        with tempfile.TemporaryDirectory(prefix="openfader-install-") as temp:
            base = Path(temp)
            repo = base / "repo"
            out = repo / "target/release"
            plugin = repo / "extension/plugin/fixture"
            for directory in (out, plugin, repo / ".github", repo / "extension/skin", repo / "mappings", repo / "assets", base / "bin"):
                directory.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / "install.sh", repo / "install.sh")
            shutil.copyfile(ROOT / ".github/bundle.py", repo / ".github/bundle.py")
            for path in (repo / "extension/plugin/build.sh", base / "bin/cargo"):
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
            (plugin / "plugin.json").write_text(json.dumps({"name": "fixture", "library": {"lib": {"linux": "libfixture.so"}}}))
            (repo / "extension/skin/fixture.json").write_text("{}")
            (repo / "mappings/fixture.xml").write_text("<MIDI/>")
            (repo / "assets/icon.png").write_bytes(b"fixture")
            source = base / "library.c"
            source.write_text("int fixture(void) { return 5; }\n")
            subprocess.run(["cc", "-shared", "-fPIC", str(source), "-o", str(plugin / "libfixture.so")], check=True)
            (out / "libfixture.so").write_bytes(b"stale companion build")
            (out / "libunrelated.so").write_bytes(b"unrelated build artifact")
            runtime_names = ["libwebgpu_dawn.so", "libonnxruntime.so.1", "libonnxruntime_providers_shared.so"] if with_runtimes else []
            for i, name in enumerate(runtime_names):
                source.write_text(f"int runtime{i}(void) {{ return {i + 1}; }}\n")
                subprocess.run(["cc", "-shared", "-fPIC", str(source), f"-Wl,-soname,{name}", "-o", str(out / name)], check=True)
            if with_runtimes:
                (out / "libonnxruntime.so").symlink_to("libonnxruntime.so.1")
            source = base / "main.c"
            source.write_text('#include <dlfcn.h>\n#include <stdio.h>\n' +
                "".join(f"int runtime{i}(void);\n" for i in range(len(runtime_names))) +
                'int main(void) { void *p = dlopen("extension/plugin/fixture/libfixture.so", RTLD_NOW); if (!p) return 2; int (*f)(void) = dlsym(p, "fixture"); if (!f) return 3; printf("%d\\n", f()' +
                "".join(f" + runtime{i}()" for i in range(len(runtime_names))) + '); dlclose(p); }\n')
            subprocess.run(["cc", str(source), *map(str, (out / name for name in runtime_names)), "-Wl,-rpath,$ORIGIN", "-ldl", "-o", str(out / "openfader")], check=True)
            config = base / "config/openfader/settings.conf"
            config.parent.mkdir(parents=True)
            config.write_text("owner settings stay here\n")
            env = dict(os.environ, PATH=str(base / "bin") + os.pathsep + os.environ["PATH"], OPENFADER_PREFIX=str(base / "prefix"), XDG_CONFIG_HOME=str(base / "config"), XDG_DATA_HOME=str(base / "data"))
            result = subprocess.run(["bash", str(repo / "install.sh")], env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            installed = base / "prefix/lib/openfader"
            result = subprocess.run([str(installed / "openfader")], cwd=installed, env={k: v for k, v in env.items() if k != "LD_LIBRARY_PATH"}, text=True, capture_output=True)
            self.assertEqual((result.returncode, result.stdout.strip()), (0, "11" if with_runtimes else "5"), result.stderr)
            self.assertFalse((installed / "libfixture.so").exists(), "companion belongs only inside its plugin folder")
            self.assertFalse((installed / "libunrelated.so").exists(), "unrelated artifacts must not ship")
            if with_runtimes:
                self.assertEqual(ctypes.CDLL(str(installed / "libonnxruntime.so")).runtime1(), 2)
            self.assertEqual(config.read_text(), "owner settings stay here\n")

    def test_runtime_dependencies_and_plugin_load_after_install(self):
        self.check_install(True)

    def test_cpu_only_install_needs_no_optional_runtime(self):
        self.check_install(False)


if __name__ == "__main__":
    unittest.main()
