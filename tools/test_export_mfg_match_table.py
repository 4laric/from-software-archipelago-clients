"""Host regression checks for the source-backed Rust identity table generator."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import export_mfg_match_table as generator


class GeneratorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.world = Path(self.temp.name)
        (self.world / "tools").mkdir()
        (self.world / "tools/export_mfg_check_registry.py").write_text(
            'import json\nINPUTS = ("input.json",)\n'
            'def build(root):\n    return json.loads((root / "input.json").read_text())\n')
        self.check = dict(ap_id=7, original_acquisition_flag=197,
                          name='A "quote" and ' + chr(92) + ' path\n雪',
                          source_identity=dict(item_lots=[dict(table="map", row_id=10180)]))
        self.write_input()

    def write_input(self):
        (self.world / "input.json").write_text(json.dumps(dict(checks=[self.check])))

    def test_check_refuses_provenance_drift_without_overwriting(self):
        output = self.world / "table.rs"
        command = [sys.executable, str(Path(generator.__file__)),
                   "--world-dir", str(self.world), "--out", str(output)]
        subprocess.run(command, check=True, capture_output=True)
        original = output.read_bytes()
        subprocess.run(command + ["--check"], check=True, capture_output=True)
        with (self.world / "input.json").open("a") as stream:
            stream.write("\n")
        result = subprocess.run(command + ["--check"], capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"differs", result.stderr)
        self.assertEqual(output.read_bytes(), original)

    def test_names_escape_rust_quotes_backslashes_and_controls(self):
        rendered = generator.render(self.world)
        slash = chr(92)
        self.assertIn('A ' + slash + '"quote' + slash + '" and '
                      + slash * 2 + ' path' + slash + 'u{a}雪', rendered)
        self.assertIn('(1, 10180, 197, 7)', rendered)
        self.assertEqual(rendered, generator.render(self.world))

    def test_integer_ranges_and_namespace_fail_early(self):
        for key, value in [("original_acquisition_flag", 2**32), ("ap_id", 2**63)]:
            old = self.check[key]
            self.check[key] = value
            self.write_input()
            with self.assertRaisesRegex(ValueError, "integer range"):
                generator.render(self.world)
            self.check[key] = old
        lot = self.check["source_identity"]["item_lots"][0]
        lot["table"] = "unknown"
        self.write_input()
        with self.assertRaisesRegex(ValueError, "Unknown lot table"):
            generator.render(self.world)


if __name__ == "__main__":
    unittest.main()
