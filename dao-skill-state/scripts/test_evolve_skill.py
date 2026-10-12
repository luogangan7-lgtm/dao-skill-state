#!/usr/bin/env python3
"""Public evolution API behavior gates. Stdlib only; called by unittest.
Owns isolated temporary fixtures, never business or installed skill data.
Checks evidence vetoes and recoverable local writes rather than prose patterns.
"""
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import evolve_skill as e


class EvolutionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.source = self.root / 'source'
        self.source.mkdir()
        (self.source / 'SKILL.md').write_text('old skill')
        (self.source / 'keep.txt').write_text('protected')
        self.a = self.root / 'attempt'
        self.a.mkdir()
        (self.a / 'input.txt').write_text('same task and oracle')
        self.plan = {'scope': 'local task', 'author': 'editor',
                     'authorization_ref': 'human local authorization',
                     'observation_ref': 'actual failure record', 'inputs': ['input.txt'],
                     'cases': [{'id': 'failure', 'must_pass': ['correct']},
                               {'id': 'success', 'must_pass': ['correct', 'critical']}]}
        self.write('plan.json', self.plan)
        self.m = e.prepare(self.source, self.a, ['SKILL.md', 'scripts/new.py'])
        (self.a / 'candidate/SKILL.md').write_text('new skill')

    def write(self, name, data):
        (self.a / name).write_text(json.dumps(data))

    def packet(self):
        candidate = e.tree_hash(e.inventory(self.a / 'candidate'))
        (self.a / 'native.log').write_text('actual execution return and artifact')
        row = lambda sha, status: {'consumer_sha256': sha, 'status': status, 'trust': 'PASS',
            'must_pass': {'correct': status}, 'evidence': ['native.log']}
        cases = [{'id': 'failure', 'baseline': row(self.m['base_sha256'], 'FAIL'),
                  'candidate': row(candidate, 'PASS')},
                 {'id': 'success', 'baseline': row(self.m['base_sha256'], 'PASS'),
                  'candidate': row(candidate, 'PASS')}]
        for arm in ('baseline', 'candidate'):
            cases[1][arm]['must_pass']['critical'] = 'PASS'
        receipt = {'base_sha256': self.m['base_sha256'], 'candidate_sha256': candidate,
                   'plan_sha256': self.m['plan_sha256'], 'inputs_sha256': self.m['inputs_sha256'],
                   'cases': cases, 'cost': {'baseline': None, 'candidate': None}}
        self.write('receipt.json', receipt)
        admission = {k: receipt[k] for k in ('base_sha256','candidate_sha256','plan_sha256','inputs_sha256')}
        admission.update({'receipt_sha256': e.file_hash(self.a / 'receipt.json'),
                          'reviewer': 'independent', 'scope': self.plan['scope'],
                          'decision': 'ACCEPT', 'rationale': 'inspected actual outputs',
                          'evidence': ['native.log'], 'efficiency_claim': False})
        self.write('admission.json', admission)
        return receipt, admission

    def bind(self):
        self.packet()
        return e.bind(self.a)

    def test_promote_fresh_consume_rollback_newfile(self):
        (self.a / 'candidate/scripts').mkdir()
        (self.a / 'candidate/scripts/new.py').write_text('print("new")')
        self.bind()
        self.assertEqual(e.verify(self.a)['status'], 'PASS')
        self.assertEqual(e.promote(self.a, self.source)['status'], 'PASS')
        self.assertEqual((self.source / 'SKILL.md').read_text(), 'new skill')
        self.assertTrue((self.source / 'scripts/new.py').is_file())
        self.assertEqual(e.rollback(self.a, self.source)['status'], 'PASS')
        self.assertFalse((self.source / 'scripts/new.py').exists())
        self.assertEqual((self.source / 'SKILL.md').read_text(), 'old skill')
        self.assertEqual((self.source / 'keep.txt').read_text(), 'protected')

    def test_evidence_and_critical_veto_and_noop(self):
        receipt, admission = self.packet()
        receipt['cases'][1]['candidate']['must_pass']['critical'] = 'FAIL'
        self.write('receipt.json', receipt)
        admission['receipt_sha256'] = e.file_hash(self.a / 'receipt.json')
        self.write('admission.json', admission)
        with self.assertRaises(e.Blocked): e.bind(self.a)
        self.bind()
        (self.a / 'native.log').unlink()
        with self.assertRaises(e.Blocked): e.verify(self.a)
        (self.a / 'candidate/SKILL.md').write_text('old skill')
        self.packet()
        with self.assertRaises(e.Blocked): e.bind(self.a)

    def test_drift_and_wrong_consumer_and_same_author(self):
        receipt, admission = self.packet()
        receipt['cases'][0]['candidate']['consumer_sha256'] = self.m['base_sha256']
        self.write('receipt.json', receipt)
        admission['receipt_sha256'] = e.file_hash(self.a / 'receipt.json')
        self.write('admission.json', admission)
        with self.assertRaises(e.Blocked): e.bind(self.a)
        self.bind()
        (self.a / 'candidate/SKILL.md').write_text('post evidence edit')
        with self.assertRaises(e.Blocked): e.verify(self.a)
        self.bind()
        (self.a / 'input.txt').write_text('changed oracle')
        with self.assertRaises(e.Blocked): e.verify(self.a)

    def test_partial_write_recovery_and_foreign_conflict(self):
        (self.a / 'candidate/keep.txt').write_text('outside allowed')
        with self.assertRaises(e.Blocked): self.bind()
        (self.a / 'candidate/keep.txt').write_text('protected')
        (self.a / 'candidate/scripts').mkdir()
        (self.a / 'candidate/scripts/new.py').write_text('new code')
        self.bind()
        real = os.replace
        def fail_new(src, dst):
            if str(dst).endswith('scripts/new.py'): raise OSError('injected second replace failure')
            return real(src, dst)
        with mock.patch.object(e.os, 'replace', side_effect=fail_new):
            with self.assertRaises(OSError): e.promote(self.a, self.source)
        self.assertEqual((self.source / 'SKILL.md').read_text(), 'new skill')
        (self.source / 'keep.txt').write_text('foreign edit')
        with self.assertRaises(e.Conflict): e.recover(self.a, self.source)
        self.assertEqual((self.source / 'SKILL.md').read_text(), 'new skill')
        (self.source / 'keep.txt').write_text('protected')
        self.assertEqual(e.recover(self.a, self.source)['status'], 'PASS')
        self.assertEqual((self.source / 'SKILL.md').read_text(), 'old skill')

    def test_source_drift_foreign_edit_no_rollback_and_symlink(self):
        self.bind()
        (self.source / 'keep.txt').write_text('foreign')
        with self.assertRaises(e.Conflict): e.promote(self.a, self.source)
        (self.source / 'keep.txt').write_text('protected')
        e.promote(self.a, self.source)
        (self.source / 'SKILL.md').write_text('another author')
        with self.assertRaises(e.Conflict): e.rollback(self.a, self.source)
        self.assertEqual((self.source / 'SKILL.md').read_text(), 'another author')
        link = self.root / 'installed'
        link.symlink_to(self.source, target_is_directory=True)
        with self.assertRaises(e.Blocked): e.rollback(self.a, link)

    def test_no_quality_gain_cost_growth_unknown_claim_and_self_admission(self):
        receipt, admission = self.packet()
        receipt['cases'][0]['baseline']['status'] = 'PASS'
        receipt['cases'][0]['baseline']['must_pass']['correct'] = 'PASS'
        receipt['cost'] = {'baseline': {'seconds': 1, 'input_tokens': 10, 'judge_seconds': 1, 'judge_input_tokens': 10},
                           'candidate': {'seconds': 8, 'input_tokens': 80, 'judge_seconds': 8, 'judge_input_tokens': 80}}
        self.write('receipt.json', receipt)
        admission['receipt_sha256'] = e.file_hash(self.a / 'receipt.json')
        self.write('admission.json', admission)
        with self.assertRaises(e.Blocked): e.bind(self.a)
        receipt, admission = self.packet()
        admission['efficiency_claim'] = True
        self.write('admission.json', admission)
        with self.assertRaises(e.Blocked): e.bind(self.a)
        admission['efficiency_claim'] = False
        admission['reviewer'] = self.plan['author']
        self.write('admission.json', admission)
        with self.assertRaises(e.Blocked): e.bind(self.a)
        self.bind()
        self.assertEqual(e.verify(self.a)['cost_status'], 'UNKNOWN')

    def test_cli_source_required_duplicate_keys_and_evidence_escape(self):
        import subprocess, sys
        self.bind()
        command = [sys.executable, '-B', str(Path(e.__file__)), 'promote', '--attempt-dir', str(self.a)]
        run = subprocess.run(command, capture_output=True, text=True, timeout=5)
        self.assertEqual(run.returncode, 1)
        self.assertEqual(json.loads(run.stdout)['status'], 'BLOCKED')
        self.assertEqual((self.source / 'SKILL.md').read_text(), 'old skill')
        receipt, admission = self.packet()
        receipt['cases'][0]['candidate']['evidence'] = ['../source/SKILL.md']
        self.write('receipt.json', receipt)
        admission['receipt_sha256'] = e.file_hash(self.a / 'receipt.json')
        self.write('admission.json', admission)
        with self.assertRaises(e.Blocked): e.bind(self.a)
        (self.a / 'receipt.json').write_text('{"x":1,"x":2}')
        with self.assertRaises(e.Blocked): e.bind(self.a)

    def test_source_mode_drift_and_rollback_do_not_overwrite_permissions(self):
        self.bind()
        original = (self.source / 'SKILL.md').stat().st_mode & 0o7777
        (self.source / 'SKILL.md').chmod(original ^ 0o040)
        changed = (self.source / 'SKILL.md').stat().st_mode & 0o7777
        with self.assertRaises(e.Conflict): e.promote(self.a, self.source)
        self.assertEqual((self.source / 'SKILL.md').stat().st_mode & 0o7777, changed)
        (self.source / 'SKILL.md').chmod(original)
        e.promote(self.a, self.source)
        (self.source / 'SKILL.md').chmod(original ^ 0o040)
        with self.assertRaises(e.Conflict): e.rollback(self.a, self.source)
        self.assertEqual((self.source / 'SKILL.md').stat().st_mode & 0o7777, changed)

    def test_candidate_mode_only_obeys_allowlist_and_bound_mode_drift(self):
        target = self.a / 'candidate/keep.txt'
        original = target.stat().st_mode & 0o7777
        target.chmod(original ^ 0o040)
        with self.assertRaises(e.Blocked): self.bind()
        target.chmod(original)
        self.bind()
        candidate = self.a / 'candidate/SKILL.md'
        candidate.chmod((candidate.stat().st_mode & 0o7777) ^ 0o040)
        with self.assertRaises(e.Blocked): e.verify(self.a)

    def test_partial_write_recover_preserves_foreign_permission_edit(self):
        (self.a / 'candidate/scripts').mkdir()
        (self.a / 'candidate/scripts/new.py').write_text('new code')
        self.bind()
        real = os.replace
        def fail_new(src, dst):
            if str(dst).endswith('scripts/new.py'): raise OSError('injected second replace failure')
            return real(src, dst)
        with mock.patch.object(e.os, 'replace', side_effect=fail_new):
            with self.assertRaises(OSError): e.promote(self.a, self.source)
        target = self.source / 'keep.txt'
        original = target.stat().st_mode & 0o7777
        target.chmod(original ^ 0o040)
        with self.assertRaises(e.Conflict): e.recover(self.a, self.source)
        self.assertEqual(target.stat().st_mode & 0o7777, original ^ 0o040)
        self.assertEqual((self.source / 'SKILL.md').read_text(), 'new skill')
        target.chmod(original)
        e.recover(self.a, self.source)
        self.assertEqual((self.source / 'SKILL.md').read_text(), 'old skill')

    def test_schema_admission_identity_and_nested_receipt_types(self):
        for key, invalid in [('reviewer', ['writer']), ('reviewer', {'name': 'reviewer'}),
                             ('rationale', {'text': 'inspected'}), ('rationale', '  ')]:
            with self.subTest(key=key, invalid=invalid):
                receipt, admission = self.packet()
                admission[key] = invalid
                self.write('admission.json', admission)
                with self.assertRaises(e.Blocked): e.bind(self.a)
        for invalid in [None, [], 'case']:
            with self.subTest(case=invalid):
                receipt, admission = self.packet()
                receipt['cases'] = [invalid]
                self.write('receipt.json', receipt)
                admission['receipt_sha256'] = e.file_hash(self.a / 'receipt.json')
                self.write('admission.json', admission)
                with self.assertRaises(e.Blocked): e.bind(self.a)
        for key, invalid in [('candidate', []), ('must_pass', ['correct']), ('evidence', {'path': 'native.log'})]:
            with self.subTest(run_field=key):
                receipt, admission = self.packet()
                if key == 'candidate': receipt['cases'][0][key] = invalid
                else: receipt['cases'][0]['candidate'][key] = invalid
                self.write('receipt.json', receipt)
                admission['receipt_sha256'] = e.file_hash(self.a / 'receipt.json')
                self.write('admission.json', admission)
                with self.assertRaises(e.Blocked): e.bind(self.a)
        self.bind()
        self.assertEqual(e.verify(self.a)['status'], 'PASS')

    def test_schema_plan_cli_is_structured_blocked_without_traceback(self):
        import subprocess, sys
        malformed = [dict(self.plan, cases=[v]) for v in (None, [], 'case')] + [None, []]
        for n, plan in enumerate(malformed):
            new = self.root / f'malformed-{n}'
            new.mkdir()
            (new / 'input.txt').write_text('frozen task')
            (new / 'plan.json').write_text(json.dumps(plan))
            command = [sys.executable, '-B', str(Path(e.__file__)), 'prepare',
                       '--source-root', str(self.source), '--attempt-dir', str(new), '--allow', 'SKILL.md']
            run = subprocess.run(command, capture_output=True, text=True, timeout=5)
            with self.subTest(plan=plan):
                self.assertEqual(run.returncode, 1)
                self.assertEqual(run.stderr, '')
                self.assertEqual(json.loads(run.stdout)['status'], 'BLOCKED')
                self.assertFalse((new / 'manifest.json').exists())

    def test_fifo_symlink_and_required_missing(self):
        (self.a / 'candidate/fifo').touch()
        (self.a / 'candidate/fifo').unlink()
        os.mkfifo(self.a / 'candidate/fifo')
        with self.assertRaises(e.Blocked): self.bind()
        (self.a / 'candidate/fifo').unlink()
        (self.a / 'candidate/link').symlink_to(self.a / 'input.txt')
        with self.assertRaises(e.Blocked): self.bind()
        (self.a / 'candidate/link').unlink()
        receipt, admission = self.packet()
        receipt['cases'].pop()
        self.write('receipt.json', receipt)
        admission['receipt_sha256'] = e.file_hash(self.a / 'receipt.json')
        self.write('admission.json', admission)
        with self.assertRaises(e.Blocked): e.bind(self.a)

if __name__ == '__main__': unittest.main()
