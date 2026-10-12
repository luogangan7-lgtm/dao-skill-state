#!/usr/bin/env python3
"""Bind local skill evolution evidence and recover narrowly scoped file updates.
Dependencies: Python stdlib, POSIX advisory flock; no models, commands or database.
Called by the Evolve route and public API tests. Owns one package-external attempt.
Invariants: ordinary files only; frozen plan/inputs; independent bound admission;
explicit verified source; no deletion of existing files or foreign-edit overwrite.
A hash binds bytes, never semantic truth or human permission. Multi-file writes
are journaled and recoverable, not atomic; non-cooperating writers remain a race.
"""
from __future__ import annotations
import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import stat
import tempfile
import uuid

MAX_BYTES = 10 * 1024 * 1024

class Blocked(ValueError):
    pass

class Conflict(Blocked):
    pass


def require(condition, message):
    if not condition:
        raise Blocked(message)


def directory(path):
    p = Path(os.path.abspath(path))
    # Check every component before resolution so installed symlink paths cannot
    # silently become verified source paths.
    for q in [p, *p.parents]:
        require(not q.is_symlink(), f'symlink directory: {q}')
    require(p.is_dir(), f'not a directory: {p}')
    return p


def relative(value):
    require(isinstance(value, str) and value, 'empty relative path')
    p = PurePosixPath(value)
    require(not p.is_absolute() and '..' not in p.parts and str(p) == value,
            f'unsafe relative path: {value}')
    return p


def inside(root, value):
    p = root.joinpath(*relative(value).parts)
    for q in [p, *list(p.parents)[:len(p.relative_to(root).parts)]]:
        require(not q.is_symlink(), f'symlink path: {q}')
    return p


def read_payload(path):
    # O_NONBLOCK prevents a raced FIFO open from hanging; O_NOFOLLOW rejects the
    # final link and fstat checks the opened object rather than a prior lstat.
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError as ex:
        raise Blocked(f'unreadable ordinary file: {path}') from ex
    try:
        s = os.fstat(fd)
        require(stat.S_ISREG(s.st_mode) and s.st_size <= MAX_BYTES, 'not bounded ordinary file')
        with os.fdopen(fd, 'rb', closefd=False) as f:
            data = f.read(MAX_BYTES + 1)
        require(len(data) <= MAX_BYTES, 'file too large')
        after = os.fstat(fd)
        require(stat.S_IMODE(s.st_mode) == stat.S_IMODE(after.st_mode), 'mode changed while reading')
        return data, stat.S_IMODE(after.st_mode)
    finally:
        os.close(fd)


def read_bytes(path):
    return read_payload(path)[0]


def file_identity(path):
    data, mode = read_payload(path)
    return {'sha256': hashlib.sha256(data).hexdigest(), 'mode': mode}


def file_hash(path):
    return hashlib.sha256(read_bytes(path)).hexdigest()


def unique(pairs):
    out = {}
    for key, value in pairs:
        require(key not in out, f'duplicate JSON key: {key}')
        out[key] = value
    return out


def load(path):
    value = json.loads(read_bytes(path), object_pairs_hook=unique,
                       parse_constant=lambda x: (_ for _ in ()).throw(Blocked('nonfinite JSON')))
    require(isinstance(value, dict), 'JSON root must be an object')
    return value


def nonblank(value):
    return isinstance(value, str) and bool(value.strip())


def tree_hash(files):
    return hashlib.sha256(json.dumps(files, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def inventory(root):
    root = directory(root)
    files = {}
    for p in sorted(root.rglob('*')):
        require(not p.is_symlink(), f'symlink payload: {p}')
        if p.is_dir():
            continue
        files[p.relative_to(root).as_posix()] = file_identity(p)
    require('SKILL.md' in files, 'source/candidate needs SKILL.md')
    require(len(files) <= 10000, 'too many payload files')
    return files


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def atomic_write(path, data, mode=0o600):
    directory(path.parent)
    require(not path.is_symlink(), 'symlink write target')
    if path.exists():
        require(stat.S_ISREG(path.lstat().st_mode), 'non-file write target')
    fd, name = tempfile.mkstemp(prefix='.dao-write-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
            os.fchmod(f.fileno(), mode)
        os.replace(name, path)
        sync_dir(path.parent)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def save(root, manifest):
    atomic_write(inside(root, 'manifest.json'), json.dumps(manifest, indent=2, sort_keys=True).encode())


def attempt(path):
    root = directory(path)
    m = load(inside(root, 'manifest.json'))
    require(m.get('schema') == 'dao-evolution/v1' and m.get('payload_identity') == 'sha256+posix-mode/v1'
            and m.get('attempt_dir') == str(root), 'attempt identity format mismatch; prepare a new attempt')
    for key in ('source_root', 'plan', 'plan_sha256', 'inputs_sha256', 'base_sha256', 'phase'):
        require(nonblank(m.get(key)), f'manifest missing string {key}')
    require(isinstance(m.get('inputs'), dict) and m['inputs']
            and all(nonblank(k) and nonblank(v) for k, v in m['inputs'].items()), 'manifest inputs must be a hash object')
    for key in ('base_files', 'candidate_files'):
        if key == 'candidate_files' and m['phase'] == 'PREPARED':
            continue
        files = m.get(key)
        require(isinstance(files, dict) and 'SKILL.md' in files, f'manifest {key} must be a payload identity object')
        for ref, identity in files.items():
            relative(ref)
            require(isinstance(identity, dict) and set(identity) == {'sha256', 'mode'}, 'invalid file identity object')
            sha, mode = identity['sha256'], identity['mode']
            require(isinstance(sha, str) and len(sha) == 64 and all(c in '0123456789abcdef' for c in sha)
                    and type(mode) is int and 0 <= mode <= 0o7777, 'invalid hash or POSIX mode')
    return root, m


@contextmanager
def locked(root):
    fd = os.open(directory(root), os.O_RDONLY | os.O_NOFOLLOW)
    try:
        require(stat.S_ISDIR(os.fstat(fd).st_mode), 'non-directory lock')
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as ex:
            raise Conflict('BUSY advisory write lock') from ex
        yield
    finally:
        os.close(fd)


def prepare(source_root, attempt_dir, allowed_paths, plan='plan.json'):
    source, root = directory(source_root), directory(attempt_dir)
    require(not root.is_relative_to(source) and not source.is_relative_to(root), 'nested source/attempt')
    require(not (root / 'manifest.json').exists(), 'attempt already prepared')
    allowed = sorted(set(str(relative(p)) for p in allowed_paths))
    require(allowed, 'allowed_paths required')
    p = load(inside(root, plan))
    for key in ('scope', 'author', 'authorization_ref', 'observation_ref'):
        require(nonblank(p.get(key)), f'plan missing string {key}')
    require(isinstance(p.get('cases'), list) and p['cases'], 'required cases missing')
    ids = set()
    for case in p['cases']:
        require(isinstance(case, dict), 'plan case must be an object')
        require(nonblank(case.get('id')) and case['id'] not in ids, 'case id missing/duplicate')
        ids.add(case['id'])
        checks = case.get('must_pass')
        require(isinstance(checks, list) and checks and all(nonblank(x) for x in checks)
                and len(checks) == len(set(checks)), 'must-pass criteria required')
    require(isinstance(p.get('inputs'), list) and p['inputs'], 'frozen task/rubric inputs required')
    inputs = {str(relative(ref)): file_hash(inside(root, ref)) for ref in p['inputs']}
    require(len(inputs) == len(p['inputs']), 'duplicate input ref')
    files = inventory(source)
    for folder in ('base', 'candidate'):
        dest = root / folder
        require(not dest.exists(), f'{folder} already exists')
        dest.mkdir()
        for ref in files:
            target = inside(dest, ref)
            target.parent.mkdir(parents=True, exist_ok=True)
            atomic_write(target, read_bytes(inside(source, ref)), stat.S_IMODE(inside(source, ref).stat().st_mode))
        require(inventory(dest) == files, 'source changed while freezing')
    require(inventory(source) == files, 'source changed while freezing')
    s = source.stat()
    m = {'schema': 'dao-evolution/v1', 'attempt_id': str(uuid.uuid4()), 'attempt_dir': str(root),
         'payload_identity': 'sha256+posix-mode/v1', 'source_root': str(source), 'source_identity': [s.st_dev, s.st_ino], 'allowed_paths': allowed,
         'base_files': files, 'base_sha256': tree_hash(files), 'plan': plan, 'plan_sha256': file_hash(inside(root, plan)),
         'inputs': inputs, 'inputs_sha256': tree_hash(inputs), 'phase': 'PREPARED'}
    save(root, m)
    return m


def check_frozen(root, m):
    require(file_hash(inside(root, m['plan'])) == m['plan_sha256'], 'plan drift')
    for ref, sha in m['inputs'].items():
        require(file_hash(inside(root, ref)) == sha, 'input/oracle drift')
    require(inventory(root / 'base') == m['base_files'], 'snapshot drift')
    return load(inside(root, m['plan']))


def packet(root, m, receipt, admission):
    p = check_frozen(root, m)
    files = inventory(root / 'candidate')
    require(set(m['base_files']) <= set(files), 'candidate deletes existing files')
    changes = [r for r in files if files[r] != m['base_files'].get(r)]
    require(changes and set(changes) <= set(m['allowed_paths']), 'no-op or changed path outside allowed scope')
    r, a = load(inside(root, receipt)), load(inside(root, admission))
    bindings = {'base_sha256': m['base_sha256'], 'candidate_sha256': tree_hash(files),
                'plan_sha256': m['plan_sha256'], 'inputs_sha256': m['inputs_sha256']}
    for key, expected in bindings.items():
        require(r.get(key) == expected and a.get(key) == expected, f'{key} binding mismatch')
    require(a.get('receipt_sha256') == file_hash(inside(root, receipt)), 'admission receipt binding mismatch')
    require(nonblank(a.get('reviewer')) and nonblank(a.get('rationale')), 'reviewer and rationale must be nonempty strings')
    require(a['reviewer'].strip() != p['author'].strip() and a.get('scope') == p['scope']
            and a.get('decision') == 'ACCEPT', 'independent scoped admission missing')
    evidence = {receipt: file_hash(inside(root, receipt)), admission: file_hash(inside(root, admission))}
    def refs(values):
        require(isinstance(values, list) and values and all(nonblank(v) for v in values), 'actual evidence refs must be nonempty string array')
        for ref in values:
            evidence[ref] = file_hash(inside(root, ref))
    refs(a.get('evidence'))
    rows = r.get('cases')
    require(isinstance(rows, list), 'case results missing')
    require(all(isinstance(row, dict) and nonblank(row.get('id')) for row in rows), 'receipt case must be an object with string id')
    results = {row['id']: row for row in rows}
    require(len(results) == len(rows) and set(results) == {c['id'] for c in p['cases']}, 'required case missing/duplicate/extra')
    gain = False
    for case in p['cases']:
        row = results[case['id']]
        for arm, sha in (('baseline', m['base_sha256']), ('candidate', bindings['candidate_sha256'])):
            run = row.get(arm)
            require(isinstance(run, dict), 'baseline/candidate run must be an object')
            require(run.get('consumer_sha256') == sha, 'actual consumer hash mismatch')
            require(run.get('trust') == 'PASS', 'Trust veto')
            require(run.get('status') in ('PASS', 'FAIL'), 'required result unassessed')
            refs(run.get('evidence'))
            checks = run.get('must_pass')
            require(isinstance(checks, dict), 'run must_pass must be an object')
            require(set(checks) == set(case['must_pass']) and all(x in ('PASS', 'FAIL') for x in checks.values()), 'must-pass results missing/unknown')
            require((run['status'] == 'PASS') == all(x == 'PASS' for x in checks.values()), 'status contradicts criterion')
            if arm == 'candidate':
                require(run['status'] == 'PASS' and all(x == 'PASS' for x in checks.values()), 'candidate critical/required failure')
        gain |= row['baseline']['status'] == 'FAIL'
    cost = r.get('cost', {})
    metrics = ('seconds', 'input_tokens', 'judge_seconds', 'judge_input_tokens')
    require(isinstance(cost, dict) and set(cost) == {'baseline', 'candidate'}, 'paired cost fields required')
    for arm in ('baseline', 'candidate'):
        values = cost[arm]
        require(values is None or isinstance(values, dict) and set(values) == set(metrics), 'invalid cost fields')
        if values is not None:
            require(all(v is None or type(v) in (int, float) and math.isfinite(v) and v >= 0
                        for v in values.values()), 'invalid cost value')
    known = all(isinstance(cost.get(arm), dict) and all(type(cost[arm].get(k)) in (int, float)
                and math.isfinite(cost[arm][k]) and cost[arm][k] >= 0 for k in metrics)
                for arm in ('baseline', 'candidate'))
    if known:
        improvements = [cost['candidate'][k] < cost['baseline'][k] for k in metrics]
        no_worse = all(cost['candidate'][k] <= cost['baseline'][k] for k in metrics)
        efficiency = no_worse and any(improvements)
    else:
        efficiency = False
    require(type(a.get('efficiency_claim')) is bool and (not a['efficiency_claim'] or efficiency), 'unmeasured/unsupported efficiency claim')
    require(gain or efficiency, 'no demonstrated gain; discard')
    return files, evidence, {'capability_gain': gain, 'cost_status': 'MEASURED' if known else 'UNKNOWN', 'efficiency_gain': efficiency}


def bind(attempt_dir, receipt='receipt.json', admission='admission.json'):
    root, m = attempt(attempt_dir)
    with locked(root):
        root, m = attempt(root)
        require(m['phase'] in ('PREPARED', 'BOUND'), 'cannot rebind pending/promoted attempt')
        files, evidence, verdict = packet(root, m, receipt, admission)
        m.update(candidate_files=files, candidate_sha256=tree_hash(files), evidence=evidence,
                 receipt=receipt, admission=admission, verdict=verdict, phase='BOUND')
        save(root, m)
    return {'status': 'PASS', 'manifest': str(root / 'manifest.json'), **verdict}


def verify(attempt_dir):
    root, m = attempt(attempt_dir)
    require(m['phase'] == 'BOUND', 'attempt not BOUND')
    files, evidence, verdict = packet(root, m, m['receipt'], m['admission'])
    require(files == m['candidate_files'] and evidence == m['evidence'], 'candidate/evidence drift; new admission needed')
    return {'status': 'PASS', 'manifest': str(root / 'manifest.json'), **verdict}


def source_bound(source_root, m):
    source = directory(source_root)
    s = source.stat()
    require(str(source) == m['source_root'] and [s.st_dev, s.st_ino] == m['source_identity'], 'explicit source identity mismatch')
    return source


def apply(root, m, source, target, recovering=False):
    current = inventory(source)
    base, candidate = m['base_files'], m['candidate_files']
    if recovering:
        valid = set(current) <= set(candidate) and set(base) <= set(current)
        valid &= all(sha in (base.get(ref), candidate.get(ref)) for ref, sha in current.items())
    else:
        valid = current == (base if target == 'candidate' else candidate)
    if not valid:
        raise Conflict('source has foreign or unexpected edits; retain and reopen')
    expected = current.copy()
    target_files = candidate if target == 'candidate' else base
    m.update(phase='PENDING', pending={'target': target, 'from_files': current})
    save(root, m)  # Durable intent and already verified snapshot precede all writes.
    for ref in sorted(set(current) | set(target_files)):
        if expected.get(ref) == target_files.get(ref):
            continue
        if inventory(source) != expected:
            raise Conflict('source changed during transaction')
        path = inside(source, ref)
        if ref in target_files:
            data_path = inside(root / target, ref)
            require(file_identity(data_path) == target_files[ref], 'transaction payload drift')
            path.parent.mkdir(parents=True, exist_ok=True)
            atomic_write(path, read_bytes(data_path), target_files[ref]['mode'])
            expected[ref] = target_files[ref]
        else:
            require(ref not in base and file_identity(path) == candidate[ref], 'not owned added file')
            path.unlink()
            sync_dir(path.parent)
            expected.pop(ref)
    if inventory(source) != target_files:
        raise Conflict('final source mismatch')
    m.update(phase='PROMOTED' if target == 'candidate' else 'ROLLED_BACK', pending=None)
    save(root, m)
    return {'status': 'PASS', 'phase': m['phase'], 'source_sha256': tree_hash(target_files), 'manifest': str(root / 'manifest.json')}


def promote(attempt_dir, source_root):
    root, m = attempt(attempt_dir)
    source = source_bound(source_root, m)
    with locked(source), locked(root):
        root, m = attempt(root)
        verify(root)
        return apply(root, m, source, 'candidate')


def recover(attempt_dir, source_root):
    root, m = attempt(attempt_dir)
    source = source_bound(source_root, m)
    with locked(source), locked(root):
        root, m = attempt(root)
        require(m['phase'] == 'PENDING', 'no pending transaction')
        check_frozen(root, m)
        return apply(root, m, source, 'base', recovering=True)


def rollback(attempt_dir, source_root):
    root, m = attempt(attempt_dir)
    source = source_bound(source_root, m)
    with locked(source), locked(root):
        root, m = attempt(root)
        require(m['phase'] == 'PROMOTED', 'attempt not PROMOTED')
        check_frozen(root, m)
        return apply(root, m, source, 'base')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('prepare', 'bind', 'verify', 'promote', 'recover', 'rollback'))
    parser.add_argument('--attempt-dir', required=True)
    parser.add_argument('--source-root')
    parser.add_argument('--allow', action='append', default=[])
    parser.add_argument('--plan', default='plan.json')
    parser.add_argument('--receipt', default='receipt.json')
    parser.add_argument('--admission', default='admission.json')
    args = parser.parse_args(argv)
    try:
        if args.action == 'prepare':
            require(args.source_root, '--source-root required')
            result = prepare(args.source_root, args.attempt_dir, args.allow, args.plan)
        elif args.action == 'bind':
            result = bind(args.attempt_dir, args.receipt, args.admission)
        elif args.action == 'verify':
            result = verify(args.attempt_dir)
        else:
            require(args.source_root, '--source-root required for source mutations')
            result = globals()[args.action](args.attempt_dir, args.source_root)
        print(json.dumps(result, sort_keys=True))
        return 0
    except (Blocked, OSError, ValueError, KeyError, TypeError, RecursionError) as ex:
        status = 'CONFLICT' if isinstance(ex, Conflict) else 'BLOCKED' if isinstance(ex, Blocked) else 'ERROR'
        print(json.dumps({'status': status, 'reason': str(ex)}))
        return 1

if __name__ == '__main__':
    raise SystemExit(main())
