import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "stage_plugins", Path(__file__).with_name("stage_plugins.py")
)
stage_plugins = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stage_plugins)


class StagePluginsTests(unittest.TestCase):
    def run_stage(self, metadata, platform, *args):
        output = io.StringIO()
        with patch.object(stage_plugins.sys, "argv", ["stage_plugins.py", *args]), \
                patch.object(stage_plugins.sys, "platform", platform), \
                patch.object(stage_plugins.subprocess, "check_output", return_value=json.dumps(metadata)), \
                contextlib.redirect_stdout(output):
            result = stage_plugins.main()
        return result, output.getvalue().strip()

    def test_stages_native_library_names_from_a_shared_target_directory(self):
        for platform, filename in [
            ("win32", "rpi_probe.dll"),
            ("linux", "librpi_probe.so"),
            ("darwin", "librpi_probe.dylib"),
        ]:
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                self.assertEqual(root.parent, Path(tempfile.gettempdir()).resolve())
                target = root / "shared-target"
                release = target / "release"
                release.mkdir(parents=True)
                (release / filename).write_bytes(b"current plugin")
                (release / "stale.dll").write_bytes(b"unrelated plugin")
                metadata = {
                    "target_directory": str(target),
                    "packages": [{"targets": [{"name": "rpi_probe", "crate_types": ["cdylib"]}]}],
                }
                staging = root / "staging"
                staging.mkdir()
                (staging / "old-plugin").write_bytes(b"stale staging content")
                result, _ = self.run_stage(metadata, platform, "--out", str(staging))
                self.assertEqual(result, 0)
                self.assertEqual([file.name for file in staging.iterdir()], [filename])
                self.assertEqual((staging / filename).read_bytes(), b"current plugin")

    def test_fixture_directory_uses_the_same_cargo_target_as_staging(self):
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary).resolve() / "shared target"
            metadata = {"target_directory": str(target)}
            result, output = self.run_stage(metadata, "win32", "--print-release-dir")
            self.assertEqual(result, 0)
            self.assertEqual(Path(output), target / "release")

    def test_rejects_output_that_would_delete_build_inputs(self):
        for destination in ["release", "target", "ancestor", "alias"]:
            with self.subTest(destination=destination), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                self.assertEqual(root.parent, Path(tempfile.gettempdir()).resolve())
                target = root / "target"
                release = target / "release"
                release.mkdir(parents=True)
                (release / "child").mkdir()
                artifact = release / "rpi_probe.dll"
                artifact.write_bytes(b"built plugin")
                metadata = {
                    "target_directory": str(target),
                    "packages": [{"targets": [{"name": "rpi_probe", "crate_types": ["cdylib"]}]}],
                }
                output = {
                    "release": release,
                    "target": target,
                    "ancestor": root,
                    "alias": release / "child" / "..",
                }[destination]
                with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                    self.run_stage(metadata, "win32", "--out", str(output))
                self.assertEqual(error.exception.code, 2)
                self.assertEqual(artifact.read_bytes(), b"built plugin")

    def test_output_inside_release_preserves_the_build_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            self.assertEqual(root.parent, Path(tempfile.gettempdir()).resolve())
            release = root / "target" / "release"
            release.mkdir(parents=True)
            artifact = release / "rpi_probe.dll"
            artifact.write_bytes(b"built plugin")
            metadata = {
                "target_directory": str(release.parent),
                "packages": [{"targets": [{"name": "rpi_probe", "crate_types": ["cdylib"]}]}],
            }
            output = release / "smoke-plugins"
            result, _ = self.run_stage(metadata, "win32", "--out", str(output))
            self.assertEqual(result, 0)
            self.assertEqual(artifact.read_bytes(), b"built plugin")
            self.assertEqual((output / artifact.name).read_bytes(), b"built plugin")

    def test_rejects_workspace_root_when_the_cargo_target_is_elsewhere(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            self.assertEqual(root.parent, Path(tempfile.gettempdir()).resolve())
            workspace = root / "workspace"
            workspace.mkdir()
            manifest = workspace / "Cargo.toml"
            manifest.write_bytes(b"workspace manifest")
            target = root / "shared-target"
            (target / "release").mkdir(parents=True)
            metadata = {
                "workspace_root": str(workspace),
                "target_directory": str(target),
                "packages": [],
            }
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                self.run_stage(metadata, "win32", "--out", str(workspace))
            self.assertEqual(error.exception.code, 2)
            self.assertEqual(manifest.read_bytes(), b"workspace manifest")


if __name__ == "__main__":
    unittest.main()
