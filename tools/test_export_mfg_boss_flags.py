import tempfile
import unittest
from pathlib import Path
from export_mfg_boss_flags import generate


class BossFlagExportTests(unittest.TestCase):
    def fixture(self, root, rewards, drops):
        folder = root / 'greenfield' / 'eldenring'
        folder.mkdir(parents=True)
        (folder / 'boss_reward_lots.py').write_text('BOSS_REWARD_DEFEAT = ' + repr(rewards))
        (folder / 'boss_drops.py').write_text('BOSS_DROP_ENTITY = ' + repr(drops))

    def test_deterministic_union_and_shared_identity(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.fixture(root, {510010: 10000800}, {530100: 1042360800, 510010: 10000800})
            output = generate(root)
            self.assertEqual(output, generate(root))
            self.assertEqual(output.count('(510010, 10000800)'), 1)
            self.assertLess(output.index('(510010,'), output.index('(530100,'))
            self.assertEqual(output.count('sha256='), 2)

    def test_conflicting_datamines_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.fixture(root, {510010: 10000800}, {510010: 999})
            with self.assertRaisesRegex(ValueError, 'Conflicting'):
                generate(root)


if __name__ == '__main__':
    unittest.main()
