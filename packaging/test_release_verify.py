#!/usr/bin/env python3
"""게시 파일 변조·부분 게시·다른 태그를 네트워크 없이 거부한다."""
import io
import json
from pathlib import Path
import plistlib
import shutil
import tarfile
import tempfile
import unittest
import zipfile

import release_verify as rv
from macos import ed25519_manifest as ed

HEAD = 'a' * 40
VERSION = '0.1.2'
REPO = 'o/r'

class PublicationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.assets = self.root / 'assets'
        self.assets.mkdir()
        self.key = self.root / 'key'
        self.seed = ed.load_or_create_seed(self.key, create_if_missing=True)
        self.pub = ed.public_key_from_seed(self.seed).hex()
        for name in ('Krill-0.1.2.dmg', 'install.sh', 'install.ps1', 'homebrew-krill/SHA256SUMS'):
            path = self.assets / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'fixture')
        info = {'CFBundleShortVersionString': VERSION, 'KrillUpdatePublicKey': self.pub}
        for name, member, content in (
            ('Krill-0.1.2-macos-arm64-adhoc.zip', 'Krill.app/Contents/Info.plist', plistlib.dumps(info)),
            ('Krill-0.1.2-windows-x64.zip', 'Krill/Krill.json', json.dumps(info).encode())):
            with zipfile.ZipFile(self.assets / name, 'w') as archive:
                archive.writestr(member, content)
            self.checksum(name)
        with tarfile.open(self.assets / f'Krill-{VERSION}-source.tar.gz', 'w:gz', format=tarfile.PAX_FORMAT,
                          pax_headers={'comment': HEAD}) as archive:
            member = tarfile.TarInfo('Krill/Cargo.toml')
            member.size = 3
            archive.addfile(member, io.BytesIO(b'abc'))
        for name, archive in (('latest.json', 'Krill-0.1.2-macos-arm64-adhoc.zip'),
                              ('latest-windows.json', 'Krill-0.1.2-windows-x64.zip')):
            manifest = {'version': VERSION, 'url': f'https://github.com/{REPO}/releases/download/v{VERSION}/{archive}',
                        'sha256': rv.sha256(self.assets / archive), 'min_daemon_protocol': 1, 'channel': 'stable', 'rollout_pct': 100}
            manifest['ed25519_sig'] = ed.ed25519_sign(self.seed, ed.canonical_signing_payload(**manifest)).hex()
            (self.assets / name).write_text(json.dumps(manifest))
        (self.assets / 'homebrew-krill/SHA256SUMS').write_text(''.join(
            rv.sha256(self.assets / name) + '  ' + name + '\n'
            for name in ('Krill-0.1.2.dmg', 'Krill-0.1.2-source.tar.gz')))
        self.plan = self.plan_for_assets()

    def checksum(self, name):
        (self.assets / (name + '.sha256')).write_text(rv.sha256(self.assets / name) + '  ' + name + '\n')

    def plan_for_assets(self):
        return rv.create_plan(self.assets, HEAD, VERSION, REPO, self.key, all_platforms=True)

    def read(self, endpoint):
        if '/git/ref/' in endpoint:
            return {'object': {'type': 'tag', 'sha': 'b' * 40}}
        if '/git/tags/' in endpoint:
            return {'object': {'type': 'commit', 'sha': HEAD}}
        return {'draft': False, 'prerelease': False, 'tag_name': 'v' + VERSION,
                'assets': [{'name': asset['name']} for asset in self.plan['assets']]}

    def verify(self, **kwargs):
        return rv.verify_public(self.plan, HEAD, REPO, self.pub, read=self.read,
            download=lambda dest: shutil.copytree(self.assets, dest, dirs_exist_ok=True), temporary_root=self.root / 'tmp', **kwargs)

    def test_complete_remote_readback_checks_every_asset(self):
        result = self.verify()
        self.assertEqual(len(result['verified_assets']), 11)
        self.assertFalse(any('linux' in name for name in result['verified_assets']))
        self.assertEqual(result['head'], HEAD)

    def test_corrupt_download_wrong_plan_signature_and_wrong_tag_fail(self):
        (self.assets / 'install.sh').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'asset bytes'):
            self.verify()
        self.plan['head'] = 'b' * 40
        with self.assertRaisesRegex(ValueError, 'does not match'):
            self.verify()
        self.plan['head'] = HEAD
        self.plan['signature'] = '00' * 64
        with self.assertRaisesRegex(ValueError, 'signature'):
            self.verify()

    def test_partial_release_and_wrong_tag_never_count_as_published(self):
        real_read = self.read
        for kind in ('tag', 'assets', 'draft'):
            def read(endpoint):
                data = real_read(endpoint)
                if kind == 'tag' and '/git/tags/' in endpoint:
                    data['object']['sha'] = 'b' * 40
                if '/releases/tags/' in endpoint:
                    if kind == 'assets': data['assets'].pop()
                    if kind == 'draft': data['draft'] = True
                return data
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                rv.verify_public(self.plan, HEAD, REPO, self.pub, read=read, temporary_root=self.root / 'tmp')

    def test_wrong_update_key_or_source_head_is_rejected_before_upload(self):
        path = self.assets / 'latest.json'
        manifest = json.loads(path.read_text())
        manifest['ed25519_sig'] = '00' * 64
        path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, 'signature'):
            self.plan_for_assets()
        path.write_text(json.dumps({**manifest, 'url': 'https://example.invalid/file'}))
        with self.assertRaisesRegex(ValueError, 'URL'):
            self.plan_for_assets()

    def test_inventory_requires_all_platforms_and_checksum_filename(self):
        path = self.assets / 'Krill-0.1.2-windows-x64.zip.sha256'
        path.write_text(rv.sha256(self.assets / path.name.removesuffix('.sha256')) + '  ../../bad')
        with self.assertRaisesRegex(ValueError, 'checksum'):
            self.plan_for_assets()
        path.unlink()
        with self.assertRaisesRegex(ValueError, 'inventory'):
            self.plan_for_assets()

if __name__ == '__main__':
    unittest.main()
