import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('publish_cli', Path(__file__).with_name('publish-cli.py'))
publish = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publish)


class PublishCliTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.previous = Path.cwd()
        os.chdir(self.directory.name)
        Path('artifacts').mkdir()
        self.tag = 'v1.0.0'
        assets = {'LICENSE'} | {
            f'zipover_{self.tag}_{target}.{extension}' for target, extension in publish.TARGETS.items()
        } | {f'THIRD-PARTY-NOTICES_{target}.txt' for target in publish.TARGETS}
        for name in assets:
            (Path('artifacts') / name).write_bytes(b'release fixture')
        self.remote = {}
        self.calls = []
        self.draft = True
        self.other_tags = []

    def tearDown(self):
        os.chdir(self.previous)
        self.directory.cleanup()

    def gh(self, *args):
        self.calls.append(args)
        if args[:2] == ('api', '--paginate'):
            return json.dumps([[{'tag_name': self.tag, 'draft': self.draft,
                                 'assets': [{'name': name} for name in self.remote]}] +
                               [{'tag_name': tag, 'draft': False, 'assets': []} for tag in self.other_tags]])
        if args[:2] == ('release', 'upload'):
            path = Path(args[3])
            self.remote[path.name] = path.read_bytes()
        elif args[:2] == ('release', 'download'):
            name = args[args.index('--pattern') + 1]
            (Path(args[args.index('--dir') + 1]) / name).write_bytes(self.remote[name])
        return ''

    def run_publish(self):
        with patch.dict(os.environ, GITHUB_REF_NAME=self.tag, GITHUB_OUTPUT='output'), patch.object(publish, 'gh', self.gh):
            publish.main()

    def test_retry_reuses_assets_and_recomputes_existing_checksums(self):
        self.run_publish()
        first = dict(self.remote)
        self.calls.clear()
        self.draft = False
        Path('artifacts/SHA256SUMS.txt').write_text('stale local checksums')
        self.run_publish()
        self.assertEqual(self.remote, first)
        self.assertFalse(any(args[:2] == ('release', 'upload') for args in self.calls))
        self.assertEqual(sum(args[:2] == ('release', 'download') for args in self.calls), 10)

    def test_changed_remote_asset_blocks_publication(self):
        self.remote['LICENSE'] = b'changed remote contents'
        with self.assertRaisesRegex(ValueError, 'refusing replacement'):
            self.run_publish()
        self.assertFalse(any(args[:2] == ('release', 'edit') for args in self.calls))

    def test_private_source_is_rejected_before_network(self):
        Path('artifacts/source.zip').write_bytes(b'private source')
        with self.assertRaisesRegex(ValueError, 'do not match'):
            self.run_publish()
        self.assertEqual(self.calls, [])

    def test_symlink_asset_is_rejected_before_network(self):
        Path('private-key').write_bytes(b'private')
        Path('artifacts/LICENSE').unlink()
        Path('artifacts/LICENSE').symlink_to('../private-key')
        with self.assertRaisesRegex(ValueError, 'regular files'):
            self.run_publish()
        self.assertEqual(self.calls, [])

    def test_historical_release_cannot_become_latest(self):
        self.other_tags = ['v1.1.0']
        self.run_publish()
        self.assertIn('--latest=false', self.calls[-1])
        self.assertEqual(Path('output').read_text(), 'latest=false\n')

    def test_latest_version_comparison_is_numeric(self):
        self.tag = 'v1.10.0'
        for path in Path('artifacts').iterdir():
            path.rename(path.with_name(path.name.replace('v1.0.0', self.tag)))
        self.other_tags = ['v1.9.0']
        self.run_publish()
        self.assertIn('--latest=true', self.calls[-1])
        self.assertEqual(Path('output').read_text(), 'latest=true\n')


if __name__ == '__main__':
    unittest.main()
