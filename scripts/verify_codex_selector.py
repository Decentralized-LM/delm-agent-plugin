#!/usr/bin/env python3
"""Exercise the installed Codex TUI and production DeLM selector in an isolated home.

Defaults to a no-model Escape/cancel proof. --case never verifies native policy
refusal; --case normal checks ordinary conversation against a loopback scripted
provider. --case worker verifies nested-worker suppression. --case positive
checks real native forks and inherited environment with scripted responses only.
--case live explicitly uses the existing account for one tiny task.
The default ceiling is 60 seconds, including owned-process cleanup. Explicit
real-model qualification can use --timeout-seconds up to 180.
Raw terminal output is evidence from real Codex, not a UI mock.
"""
import argparse
import gzip
import shlex
import sys
import fcntl
import hashlib
import http.server
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import stat
import struct
import subprocess
import termios
import threading
import time

from build import stage_package
from verify_fresh_install import identity_running, redacted
from verify_native_lifecycle import RPC

SOURCE = Path(__file__).resolve().parent.parent
LIVE_TASK = ('$delm:run Create hello.txt containing exactly "Hello from DeLM." followed by a newline. '
             'This is the whole task. One agent should do the focused file-content check once. '
             'Finish immediately after publishing and delivering the file. Do not add tests or other files.')


def save(path, value):
    path.write_text(json.dumps(redacted(value), indent=2) + '\n')


def read_json(path):
    try:
        return json.loads(path.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def invocation_records(project):
    """Read only captures for this deliberately created qualification project."""
    lifecycle = Path('/tmp').resolve() / f'delm-{os.getuid()}' / 'lifecycle'
    for path in lifecycle.glob('input-*.json'):
        capture = read_json(path) or {}
        if capture.get('project') == str(project):
            yield path, capture


def owned_identities(runs, project):
    identities = {}
    for path in runs.glob('*/launches/*/watchdog.json'):
        record = read_json(path) or {}
        for key in ['host', 'runtime']:
            item = record.get(key)
            if item:
                identities[item['pid']] = item
    for path in runs.glob('*/launches/*/shutdown-report.json'):
        for item in (read_json(path) or {}).get('owned_processes', []):
            identities[item['pid']] = item
    # Captured runtimes can exist briefly before the per-run watchdog is saved.
    for path, _capture in invocation_records(project):
        launch = read_json(path.with_suffix('.launch.json')) or {}
        item = launch.get('process')
        if item:
            identities[item['pid']] = item
    return list(identities.values())


def stop_process_group(process, remaining):
    """Reap an owned launcher even when macOS refuses to signal its exit race."""
    for sig in (signal.SIGTERM, signal.SIGKILL):
        if process.poll() is not None:
            return
        denied = None
        try:
            os.killpg(process.pid, sig)
        except ProcessLookupError:
            pass
        except PermissionError as error:
            # macOS can return EPERM for a process already exiting. Only a
            # successful wait proves this benign; a live process still fails.
            denied = error
        try:
            process.wait(timeout=remaining(.5))
            return
        except subprocess.TimeoutExpired:
            if denied is not None:
                raise denied
            if sig == signal.SIGKILL:
                raise


def wait_for_owned_shutdown(runs, project, deadline):
    """Let the runtime acknowledge native shutdown before forcing its children."""
    while True:
        identities = owned_identities(runs, project)
        if all(not identity_running(item) for item in identities):
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(.05)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, default=SOURCE / 'target/debug/delm')
    parser.add_argument('--codex', default=shutil.which('codex'))
    parser.add_argument('--case', choices=['cancel', 'never', 'normal', 'worker', 'positive', 'live'], default='cancel')
    parser.add_argument('--agents', type=int, choices=[2, 3, 4], default=2)
    parser.add_argument('--auth-home', type=Path)
    parser.add_argument('--timeout-seconds', type=int, default=60)
    args = parser.parse_args()
    if not 60 <= args.timeout_seconds <= 180 or (args.case != 'live' and args.timeout_seconds != 60):
        parser.error('Timeout must be 60 seconds, or 60..180 for an explicitly authorized live test')
    if not args.codex or not args.runtime.is_file():
        parser.error('Installed Codex and a built --runtime are required')
    if args.case == 'live' and (not args.auth_home or not (args.auth_home / 'auth.json').is_file()):
        parser.error('--case live requires --auth-home with an existing file-backed login')
    root = args.out.resolve()
    if root.exists() or (args.auth_home and root.is_relative_to(args.auth_home.resolve())):
        parser.error('--out must be a new directory outside the native account home')
    root.mkdir(parents=True, mode=0o700)
    home, project = root / 'home', root / 'project'
    home.mkdir(mode=0o700)
    project.mkdir()
    runs = home / 'Library/Application Support/DeLM/runs'
    requests, raw, clients = [], bytearray(), []
    provider_stop = threading.Event()
    worker_tools_observed = set()
    worker_coordination_observed = set()
    scripted_turns = {}
    status_revisions = {}
    child = provider = master = None
    started = time.monotonic()
    evidence = {'case': args.case, 'agents': args.agents, 'host_version': subprocess.check_output(
        [args.codex, '--version'], text=True, timeout=3).strip(),
        'runtime_sha256': hashlib.sha256(args.runtime.read_bytes()).hexdigest(),
        'model': 'gpt-6-astra' if args.case == 'live' else 'scripted-no-model',
        'effort': 'medium', 'passed': False}
    environment = {'PATH': os.environ['PATH'], 'HOME': str(home), 'CODEX_HOME': str(home),
                   'SHELL': '/bin/zsh', 'LANG': 'en_US.UTF-8', 'TERM': 'xterm-256color',
                   'TMPDIR': os.environ.get('TMPDIR', '/tmp'), 'NO_COLOR': '1'}
    environment['DELM_NATIVE_ENV_PROOF'] = 'delm-selector-inherited-environment'
    if args.case == 'worker':
        environment['DELM_WORKER_SESSION'] = '1'
    def hard_deadline(_signum, _frame):
        raise TimeoutError('Native selector qualification stopped work to preserve its total budget')
    signal.signal(signal.SIGALRM, hard_deadline)
    # Reserve ten seconds for bounded cleanup even if setup blocks.
    signal.setitimer(signal.ITIMER_REAL, args.timeout_seconds - 10)
    try:
        subprocess.run(['/usr/bin/git', '-c', 'maintenance.auto=false', '-c', 'gc.auto=0',
                        'init', '-q', '--template=', str(project)], env=dict(environment,
                        GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null'), check=True, timeout=3)
        policy = 'never' if args.case == 'never' else 'on-request'
        config = (f'model = "gpt-6-astra"\nmodel_reasoning_effort = "medium"\n'
                  f'approval_policy = "{policy}"\nsandbox_mode = "danger-full-access"\n'
                  'cli_auth_credentials_store = "file"\nweb_search = "disabled"\n'
                  'allow_login_shell = false\ncheck_for_update_on_startup = false\n')
        if args.case != 'live':
            class Provider(http.server.BaseHTTPRequestHandler):
                def log_message(self, *_):
                    pass
                def do_POST(self):
                    body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
                    payload = json.loads(gzip.decompress(body) if self.headers.get('Content-Encoding') == 'gzip' else body)
                    schema_fields = payload.get('text', {}).get('format', {}).get('schema', {}).get('properties', {})
                    title_requested = set(schema_fields) == {'title'}
                    matched = re.search(r'You are worker (\d+) of (\d+)', json.dumps(payload))
                    worker = int(matched.group(1)) if matched else 0
                    if not title_requested:
                        scripted_turns[worker] = scripted_turns.get(worker, 0) + 1
                    observed = any(item.get('type') == 'function_call_output'
                        and 'delm-selector-inherited-environment' in str(item.get('output', ''))
                        for item in payload.get('input', []) if isinstance(item, dict))
                    if worker and observed:
                        worker_tools_observed.add(worker)
                    if worker and any(item.get('type') in ['function_call_output', 'custom_tool_call_output']
                            and all(key in str(item.get('output', '')) for key in ['recent_commands', 'request_revision', 'workers'])
                            for item in payload.get('input', []) if isinstance(item, dict)):
                        worker_coordination_observed.add(worker)
                    requests.append({'at_seconds': time.monotonic() - started,
                                     'path': self.path, 'request_bytes': len(body), 'worker': worker,
                                     'model': payload.get('model'),
                                     'title_schema_requested': title_requested,
                                     'input_item_types': [item.get('type', item.get('role', 'unknown'))
                                         for item in payload.get('input', []) if isinstance(item, dict)],
                                     'native_session_id': self.headers.get('session_id'),
                                     'native_turn_id': self.headers.get('x-codex-turn-id'),
                                     'environment_observed_by_native_tool': observed})
                    events = [{'type':'response.created','response':{'id':'fixture'}}]
                    if title_requested:
                        events.append({'type':'response.output_item.done','item':{'type':'message',
                            'role':'assistant','id':'fixture-title','content':[{'type':'output_text',
                            'text':'{"title":"DeLM selector qualification"}'}]}})
                    elif args.case == 'positive':
                        if scripted_turns[worker] == 1:
                            if worker:
                                command = shlex.quote(sys.executable) + ' -c ' + shlex.quote(
                                    'import os; print(os.environ.get("DELM_NATIVE_ENV_PROOF", "MISSING"))')
                            else:
                                capture = next(invocation_records(project))[0]
                                runtime = next((home / 'plugins/cache/selector-proof/delm').glob('*/bin/delm'))
                                command = shlex.quote(str(runtime)) + ' follow --capture ' + shlex.quote(str(capture))
                            events.append({'type':'response.output_item.done','item':{'type':'function_call',
                                'call_id':f'fixture-native-{worker}','name':'exec_command',
                                'arguments':json.dumps({'cmd':command,'yield_time_ms':1000,'max_output_tokens':1000})}})
                        elif worker and scripted_turns[worker] == 2:
                            events.append({'type':'response.output_item.done','item':{'type':'custom_tool_call',
                                'call_id':f'fixture-board-{worker}',
                                'name':'exec', 'namespace':'functions',
                                'input':f'text(await tools.mcp__delm_coordination_{worker}__delm_status({{}}));'}})
                        else:
                            # Keep the native turn open while the bounded harness
                            # observes all workers, rather than triggering Stop.
                            provider_stop.wait(20)
                    events.append({'type':'response.completed','response':{'id':'fixture','usage':{
                        'input_tokens':0,'output_tokens':0,'total_tokens':0}}})
                    stream = ''.join('data: ' + json.dumps(event) + '\n\n' for event in events).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                    self.send_header('Content-Length', str(len(stream)))
                    self.end_headers()
                    try:
                        self.wfile.write(stream)
                    except (BrokenPipeError, ConnectionResetError):
                        pass
            provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
            threading.Thread(target=provider.serve_forever, daemon=True).start()
            config += ('model_provider = "fixture"\n[model_providers.fixture]\nname = "No-model fixture"\n'
                       f'base_url = "http://127.0.0.1:{provider.server_port}/v1"\nwire_api = "responses"\n'
                       'requires_openai_auth = false\nrequest_max_retries = 0\nstream_max_retries = 0\n')
        config += ('[features]\nplugins = true\nhooks = true\nmemories = false\nshell_snapshot = false\n'
                   'multi_agent = false\nmulti_agent_v2 = false\ncode_mode = false\napps = false\n'
                   f'[projects.{json.dumps(str(project))}]\ntrust_level = "trusted"\n')
        (home / 'config.toml').write_text(config)
        if args.case == 'live':
            (home / 'auth.json').symlink_to(args.auth_home.resolve() / 'auth.json')
        package = root / 'marketplace/plugin'
        stage_package(SOURCE, args.runtime.resolve(), package)
        catalog = root / 'marketplace/.agents/plugins'
        catalog.mkdir(parents=True)
        save(catalog / 'marketplace.json', {'name': 'selector-proof', 'plugins': [
            {'name': 'delm', 'source': {'source': 'local', 'path': './plugin'}}]})
        for command in [['marketplace', 'add', str(root / 'marketplace')], ['add', 'delm@selector-proof']]:
            subprocess.run([args.codex, 'plugin', *command], env=environment, cwd=root,
                           capture_output=True, text=True, check=True, timeout=5)
        discovery = RPC(args.codex, home, root, 'discovery')
        clients.append(discovery)
        listing = discovery.request('hooks/list', {'cwds': [str(project)]}, timeout=5)
        save(root / 'hooks-listing.json', listing)
        discovery.close()
        config = (home / 'config.toml').read_text()
        for item in listing['data']:
            for hook in item['hooks']:
                config += f'\n[hooks.state.{json.dumps(hook["key"])}]\ntrusted_hash = {json.dumps(hook["currentHash"])}\n'
        (home / 'config.toml').write_text(config)
        prompt = LIVE_TASK if args.case == 'live' else ('$delm:run Harmless selector fixture.'
                  if args.case != 'normal' else 'Harmless ordinary fixture prompt.')
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 110, 0, 0))
        child = subprocess.Popen([args.codex, '--no-daemon', '--no-alt-screen', '-C', str(project), prompt],
                                 stdin=slave, stdout=slave, stderr=slave, env=environment, start_new_session=True)
        os.close(slave)
        form_at = answer_at = None
        while time.monotonic() - started < args.timeout_seconds - 15:
            if select.select([master], [], [], .03)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                raw.extend(data)
                if b'\x1b[6n' in data:
                    os.write(master, b'\x1b[1;1R')
                if b'\x1b[c' in data:
                    os.write(master, b'\x1b[?1;2c')
            if b'How many agents?' in raw and form_at is None:
                form_at = time.monotonic()
                evidence['selector_visible_seconds'] = form_at - started
            if form_at and answer_at is None and time.monotonic() - form_at > .5:
                (root / 'form.raw').write_bytes(raw)
                keys = b'\x1b[B' * (args.agents - 2) + b'\r' if args.case in ['positive', 'live'] else b'\x1b'
                os.write(master, keys)
                answer_at = time.monotonic()
                evidence['confirmation_seconds'] = answer_at - started
            if args.case in ['normal', 'worker'] and requests:
                break
            if args.case in ['cancel', 'never'] and b'No agents started' in raw:
                break
            if args.case in ['positive', 'live']:
                for status_path in runs.glob('*/runtime-status.json'):
                    status = read_json(status_path) or {}
                    revision = status.get('update_sequence')
                    if revision is not None and status_revisions.get(str(status_path)) != revision:
                        status_revisions[str(status_path)] = revision
                        evidence.setdefault('status_changes', []).append({
                            'at_seconds': time.monotonic() - started,
                            'status': status.get('status'), 'message': status.get('message')})
                startup_errors = []
                for capture_path, capture in invocation_records(project):
                    evidence['confirmed_count'] = capture.get('worker_count')
                    event_path = capture_path.with_suffix('.events.jsonl')
                    if event_path.exists():
                        for line in event_path.read_text().splitlines():
                            try:
                                event = json.loads(line)
                            except json.JSONDecodeError:
                                continue
                            if event.get('type') == 'error':
                                startup_errors.append(event.get('message'))
                if startup_errors:
                    evidence['runtime_start_errors'] = startup_errors
                    break
                saved_runs = [value for path in runs.glob('*/run.json') if (value := read_json(path))]
                if (args.case == 'positive' and 'metadata_refresh_seconds' not in evidence
                        and any(run.get('status') == 'running' for run in saved_runs)):
                    # Reproduce filesystem/Finder bookkeeping on this fixture's
                    # installed resources, without changing their trusted bytes.
                    for relative in ['hooks/hooks.json', 'bin/delm']:
                        resource = next((home / 'plugins/cache/selector-proof/delm').glob(f'*/{relative}'))
                        os.chflags(resource, resource.stat().st_flags ^ stat.UF_HIDDEN)
                    evidence['metadata_refresh_seconds'] = time.monotonic() - started
                if (args.case == 'positive'
                        and worker_tools_observed == worker_coordination_observed == set(range(1, args.agents + 1))
                        and time.monotonic() - started > evidence.get('metadata_refresh_seconds', float('inf')) + .5):
                    break
                if saved_runs and saved_runs[-1].get('status') in ['delivered', 'complete', 'completed', 'failed', 'stopped', 'recovery_required']:
                    evidence['runtime_status'] = saved_runs[-1]['status']
                    break
            if child.poll() is not None:
                break
        evidence['form_displayed'] = form_at is not None
        evidence['provider_requests_before_confirmation'] = None if args.case == 'live' else sum(
            request['at_seconds'] < evidence.get('confirmation_seconds', float('inf'))
            for request in requests)
        if args.case in ['cancel', 'never']:
            evidence['passed'] = not requests and b'No agents started' in raw and not list(runs.glob('*/run.json'))
            if args.case == 'cancel':
                evidence['passed'] &= form_at is not None
        elif args.case in ['normal', 'worker']:
            evidence['passed'] = bool(requests) and form_at is None and not list(runs.glob('*/run.json'))
        else:
            for path in runs.glob('*/run.json'):
                saved = read_json(path) or {}
                delivery = read_json(path.parent / 'workspace/delivery/result.json') or {}
                evidence['run'] = {'id': path.parent.name, 'status': saved.get('status'),
                    'worker_count': len(saved.get('workers', [])), 'selected_count': saved.get('request', {}).get('worker_count'),
                    'model': saved.get('request', {}).get('model'), 'effort': saved.get('request', {}).get('reasoning_effort'),
                    'auth_home': saved.get('request', {}).get('auth_home'),
                    'worker_threads': [worker.get('thread') for worker in saved.get('workers', [])],
                    'delivery': delivery}
            hello = project / 'hello.txt'
            evidence['native_tool_environment_workers'] = sorted(worker_tools_observed)
            evidence['native_coordination_workers'] = sorted(worker_coordination_observed)
            evidence['passed'] = (hello.is_file() and hello.read_bytes() == b'Hello from DeLM.\n'
                and evidence.get('run', {}).get('worker_count') == args.agents
                and evidence.get('run', {}).get('selected_count') == args.agents
                and evidence.get('run', {}).get('model') == 'gpt-6-astra'
                and evidence.get('run', {}).get('effort') == 'medium'
                and evidence.get('run', {}).get('delivery', {}).get('delivered') is True)
            if args.case == 'live':
                run = evidence.get('run', {})
                evidence['passed'] &= (len(set(run.get('worker_threads', []))) == args.agents
                    and all(run.get('worker_threads', [])) and run.get('auth_home') == str(home)
                    and evidence.get('confirmed_count') == args.agents
                    and run.get('delivery', {}).get('cleanup_complete') is True)
                evidence['worker_workspaces_removed'] = not any(
                    path.exists() for run_path in runs.glob('*/run.json')
                    for path in [run_path.parent / 'workspace/baseline',
                                 *(run_path.parent / f'workspace/worker-{index}' for index in range(1, args.agents + 1))])
                evidence['passed'] &= evidence['worker_workspaces_removed']
            if args.case == 'positive':
                run = evidence.get('run', {})
                evidence['passed'] = (run.get('worker_count') == args.agents and run.get('selected_count') == args.agents
                    and len(set(run.get('worker_threads', []))) == args.agents
                    and all(run.get('worker_threads', [])) and run.get('auth_home') == str(home)
                    and evidence.get('confirmed_count') == args.agents
                    and evidence['provider_requests_before_confirmation'] == 0
                    and worker_tools_observed == worker_coordination_observed == set(range(1, args.agents + 1)))
    except Exception as error:
        evidence['error'] = str(error)
    finally:
        # Cleanup continues after an individual failure; one dead process or
        # closed stream must not skip the remaining owned children/auth link.
        signal.setitimer(signal.ITIMER_REAL, 0)
        provider_stop.set()
        cleanup_errors = []
        def attempt(label, action):
            try:
                return action()
            except ProcessLookupError:
                return None  # An owned child already exited between checks.
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                cleanup_errors.append(f'{label}: {error}')
                return None
        def remaining(limit):
            return max(.01, min(limit, started + args.timeout_seconds - 2 - time.monotonic()))
        def stop_group(process):
            attempt('stop native process group', lambda: stop_process_group(process, remaining))
        # Stop only runs and processes rooted in this disposable qualification.
        for path in runs.glob('*/run.json'):
            attempt('stop qualification run', lambda: subprocess.run(
                [str(args.runtime.resolve()), 'stop', '--run-id', path.parent.name],
                env=environment, capture_output=True, timeout=remaining(2)))
        evidence['cooperative_shutdown'] = attempt('wait for native shutdown', lambda:
            wait_for_owned_shutdown(runs, project, min(started + args.timeout_seconds - 5, time.monotonic() + 10)))
        if child:
            stop_group(child)
        identities = attempt('read owned process identities', lambda: owned_identities(runs, project))
        identities_known = identities is not None
        identities = identities or []
        for item in identities:
            if identity_running(item):
                attempt('terminate owned process', lambda: os.kill(item['pid'], signal.SIGTERM))
        cleanup_deadline = min(started + args.timeout_seconds - 3, time.monotonic() + 3)
        while time.monotonic() < cleanup_deadline and any(identity_running(item) for item in identities):
            time.sleep(.05)
        for item in identities:
            if identity_running(item):
                attempt('kill owned process', lambda: os.kill(item['pid'], signal.SIGKILL))
        kill_deadline = min(started + args.timeout_seconds - 2, time.monotonic() + .5)
        while time.monotonic() < kill_deadline and any(identity_running(item) for item in identities):
            time.sleep(.01)
        for client in clients:
            stop_group(client.process)
            attempt('close discovery log', client.log.close)
        evidence['owned_processes_stopped'] = (identities_known
            and all(not identity_running(item) for item in identities)
            and (not child or child.poll() is not None)
            and all(client.process.poll() is not None for client in clients))
        auth = home / 'auth.json'
        if auth.is_symlink() and evidence['owned_processes_stopped']:
            attempt('remove temporary login link', auth.unlink)
        if master is not None:
            attempt('close native terminal', lambda: os.close(master))
        if provider:
            attempt('stop scripted provider', provider.shutdown)
            attempt('close scripted provider', provider.server_close)
        if cleanup_errors:
            evidence['cleanup_errors'] = cleanup_errors
        if not evidence['owned_processes_stopped'] or cleanup_errors:
            evidence['passed'] = False
        evidence['final_run_statuses'] = [(read_json(path) or {}).get('status')
                                        for path in runs.glob('*/run.json')]
        evidence['auth_reference_removed'] = not auth.is_symlink()
        if args.case == 'live' and evidence['final_run_statuses'] not in [['complete'], ['delivered']]:
            evidence['passed'] = False
        if args.case == 'positive' and (not evidence['cooperative_shutdown']
                                       or evidence['final_run_statuses'] != ['stopped']):
            evidence['passed'] = False
        evidence['elapsed_seconds'] = time.monotonic() - started
        evidence['provider_requests'] = requests
        (root / 'terminal.raw').write_bytes(raw)
        text = re.sub(r'\x1b\][^\x07]*(?:\x07|\x1b\\)', '', raw.decode(errors='replace'))
        text = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', text)
        (root / 'terminal.txt').write_text(text)
        save(root / 'result.json', evidence)
        print(json.dumps(redacted(evidence), indent=2))
    return 0 if evidence['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
