#!/usr/bin/env python3
"""Disposable GitHub runner only: never touches the developer's signing keychain."""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys

if os.environ.get('GITHUB_ACTIONS') != 'true':
    raise SystemExit('diagnostic is restricted to a disposable GitHub runner')
sys.path.insert(0, 'packaging/macos')
from test_pipeline import PipelineTests


def call(args):
    result = subprocess.run(list(map(str, args)), capture_output=True, text=True, timeout=45)
    print(json.dumps({'command': list(map(str, args)), 'exit': result.returncode,
                      'stdout': result.stdout, 'stderr': result.stderr}), flush=True)
    return result


original = shlex.split(subprocess.check_output(['/usr/bin/security', 'list-keychains', '-d', 'user'], text=True))
PipelineTests.setUpClass()
test = PipelineTests('test_archive_roundtrip_and_source_binaries_unchanged')
test.setUp()
try:
    result = test.package()
    print(json.dumps({'phase': 'actual_bundle', 'exit': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}), flush=True)
    call(['/usr/bin/security', 'find-identity', '-p', 'codesigning', test.keychain])
    cert = subprocess.check_output(['/usr/bin/security', 'find-certificate', '-c',
        'Minikrill Local Code Signing', '-Z', test.keychain], text=True)
    digest = next(line.split()[-1] for line in cert.splitlines() if line.startswith('SHA-1 hash:'))
    for scope in ('original_search', 'fixture_search'):
        if scope == 'fixture_search':
            call(['/usr/bin/security', 'list-keychains', '-d', 'user', '-s', str(test.keychain), *original])
        for selector in ('Minikrill Local Code Signing', digest):
            destination = test.output / ('probe-' + scope + ('-sha' if selector == digest else '-name'))
            shutil.copyfile(test.bins / 'krill-app', destination)
            print(json.dumps({'phase': scope, 'selector': 'sha' if selector == digest else 'name'}), flush=True)
            call(['/usr/bin/codesign', '--force', '--keychain', test.keychain, '--sign', selector,
                  '--identifier', 'dev.krill.diagnostic', destination])
finally:
    call(['/usr/bin/security', 'list-keychains', '-d', 'user', '-s', *original])
    test.doCleanups()
    PipelineTests.tearDownClass()
