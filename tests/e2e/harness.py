"""What the end-to-end tests share: workstations in scratch directories, the
test host, a link to its rookeryd, the rookery serve of each workstation on a
database of its own, an OAuth issuer and a small JSON-RPC client over /ws.

The tests run in the runner container of ci/compose.yml, next to
the test host `studio`, on a network that reaches nothing else. Everything a
test writes goes into scratch directories; the checkout is read-only.
"""
import base64
import contextlib
import fcntl
import gzip
import hashlib
import http.client
import io
import json
import os
import pty
import re
import secrets
import select
import shutil
import signal
import socket
import struct
import subprocess
import tarfile
import tempfile
import threading
import time
import unittest
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SRC = Path(os.environ.get('ROOKERY_E2E_SOURCE', '/src'))
ROOKERY = SRC / 'target/debug/rookery'
HOST = os.environ.get('ROOKERY_E2E_HOST', 'studio')
SEED = Path(os.environ.get('ROOKERY_E2E_SEED', '/seed'))
OPERATOR, AGENT = 'ops', 'agent'

# Everything scratch lives below this, removed when the run ends.
SCRATCH = Path(tempfile.mkdtemp(prefix='rookery-e2e-'))

# The PostgreSQL the servers of the tests keep their state in, a database each.
POSTGRES = os.environ.get('ROOKERY_E2E_POSTGRES', 'postgres://rookery:rookery-e2e@database:5432/rookery')

# The servers that run, stopped when the run ends.
_servers = []


def make_database(name):
    """A new, empty database on POSTGRES for one server: its URL."""
    url = urllib.parse.urlsplit(POSTGRES)
    database = f'e2e_{re.sub(r"[^a-z0-9]", "_", name.lower())}_{secrets.token_hex(4)}'
    subprocess.run(['createdb', '-h', url.hostname, '-p', str(url.port or 5432), '-U', url.username, database],
                   env={**os.environ, 'PGPASSWORD': url.password or ''}, check=True, capture_output=True,
                   timeout=60)
    return urllib.parse.urlunsplit(url._replace(path=f'/{database}'))

# Each Ansible run of a workstation or a rookery serve, noted by the callback in
# fixtures/callbacks, and what the run made: for the summary run.py prints.
APPLIES = SCRATCH / 'applies.jsonl'
MADE = {'workstation': 0, 'serve': 0}


def counts():
    """How many playbook runs, their seconds, the tasks that took longest in
    them, and the workstations and servers made so far."""
    runs = [json.loads(line) for line in APPLIES.read_text().splitlines()] if APPLIES.exists() else []
    tasks = {}
    for run in runs:
        for seconds, name in run.get('slowest', []):
            tasks[name] = tasks.get(name, 0) + seconds
    return {'apply': len(runs), 'apply_seconds': sum(run['seconds'] for run in runs), 'tasks': tasks, **MADE}


def runtime_dir():
    """A runtime directory: short, for the sockets of ssh's shared connections in it."""
    return Path(tempfile.mkdtemp(prefix='r', dir=SCRATCH.parent))


def needs_e2e(cls):
    """Skip a test case outside the runner container."""
    reason = 'the end-to-end tests run in containers: ci/run.sh e2e'
    return unittest.skipUnless(os.environ.get('ROOKERY_E2E') == '1', reason)(cls)


def wait_for(what, check, timeout=30.0, every=0.25):
    """Call check until it returns something true, and return that."""
    end = time.monotonic() + timeout
    last = None
    while time.monotonic() < end:
        try:
            found = check()
            if found:
                return found
        except Exception as error:  # noqa: BLE001 (kept to say what went wrong last)
            last = error
        time.sleep(every)
    raise AssertionError(f'{what} did not happen within {timeout:.0f}s' + (f': {last!r}' if last else ''))


def keygen(path, comment):
    subprocess.run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-C', comment, '-f', str(path)],
                   check=True, stdin=subprocess.DEVNULL)
    return Path(f'{path}.pub').read_text().strip()


# ---------------------------------------------------------------------------
# The provisioning source rookery applies


def source():
    """The checkout with tests/e2e/site.yml as provision/ansible/site.yml, made once."""
    overlay = SCRATCH / 'source'
    if overlay.exists():
        return overlay
    overlay.mkdir()
    for entry in SRC.iterdir():
        # A virtual environment or build output of the checkout is not ours.
        if entry.name.startswith('.') or entry.name in ('provision', 'target', 'build'):
            continue
        (overlay / entry.name).symlink_to(entry)
    (overlay / 'provision').mkdir()
    for entry in (SRC / 'provision').iterdir():
        if entry.name != 'ansible':
            (overlay / 'provision' / entry.name).symlink_to(entry)
    (overlay / 'provision/ansible').mkdir()
    (overlay / 'provision/ansible/roles').symlink_to(SRC / 'provision/ansible/roles')
    (overlay / 'provision/ansible/site.yml').symlink_to(SRC / 'tests/e2e/site.yml')
    # The controller alone is installed by the role's own playbook for it.
    (overlay / 'provision/ansible/controller.yml').symlink_to(SRC / 'provision/ansible/controller.yml')
    return overlay


def source_with_plugins(name, plugins):
    """Another overlay of the checkout whose registry of the plugins that
    come with rookery, plugins/, has these besides its own, by id: {id:
    package folder of the group rookery}."""
    overlay = SCRATCH / name
    overlay.mkdir()
    for entry in source().iterdir():
        if entry.name != 'plugins':
            (overlay / entry.name).symlink_to(entry.resolve() if entry.name != 'provision' else entry)
    (overlay / 'plugins/rookery').mkdir(parents=True)
    for shipped in (SRC / 'plugins/rookery').iterdir():
        (overlay / 'plugins/rookery' / shipped.name).symlink_to(shipped)
    shutil.copy(SRC / 'plugins/index.json', overlay / 'plugins/index.json')
    for folder in plugins.values():
        publish_builtin(overlay / 'plugins', folder)
    return overlay


def publish_builtin(registry, folder):
    """Publish a package folder of the group rookery in a static registry,
    as an egg beside its index: what a new rookery brings. The registry
    keeps only this version of it."""
    kdl = (folder / 'package.kdl').read_text()
    plugin = re.search(r'^\s*id (\S+)', kdl, re.M).group(1)
    version = re.search(r'^\s*version "([^"]+)"', kdl, re.M).group(1)
    data = io.BytesIO()
    with gzip.GzipFile(fileobj=data, mode='wb', mtime=0) as gz, tarfile.open(fileobj=gz, mode='w') as egg:
        for path in sorted([folder / 'package.kdl', *(p for p in (folder / 'rookery').rglob('*') if p.is_file())]):
            info = tarfile.TarInfo(str(path.relative_to(folder)))
            info.size = path.stat().st_size
            info.mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
            with path.open('rb') as stream:
                egg.addfile(info, stream)
    package = registry / 'rookery' / plugin
    if package.is_symlink():
        package.unlink()
    package.mkdir(parents=True, exist_ok=True)
    for old in package.glob('*.egg'):
        old.unlink()
    (package / f'{plugin}-{version}.egg').write_bytes(data.getvalue())
    (package / 'index.json').write_text(json.dumps({
        'group': 'rookery', 'id': plugin,
        'versions': {version: {
            'sha256': hashlib.sha256(data.getvalue()).hexdigest(),
            'published': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
            'targets': {'rookery': {'requires': {'rookery': '>=0.6'}}}}}}, indent=2))
    catalog = json.loads((registry / 'index.json').read_text())
    catalog['packages'] = sorted(set(catalog['packages']) | {f'rookery/{plugin}'})
    (registry / 'index.json').write_text(json.dumps(catalog, indent=2))


# ---------------------------------------------------------------------------
# A workstation: a HOME and XDG directories of its own


def follows_a_job(args):
    """Whether rookery follows a job of its server for these arguments."""
    words = [arg for arg in args if not str(arg).startswith('-')]
    first, second = (words + ['', ''])[:2]
    return (first in ('apply', 'rollout')
            or (first, second) in {('host', 'add'), ('host', 'apply'), ('host', 'adopt'), ('host', 'iso'), ('host', 'create'),
                                   ('host', 'rename'), ('host', 'start'), ('host', 'stop'), ('key', 'trust'),
                                   ('key', 'rotate'), ('resource', 'create'), ('resource', 'set'),
                                   ('resource', 'remove'), ('resource', 'run'), ('provisioner', 'run'),
                                   ('provisioner', 'set'), ('provisioner', 'rename')})


class Workstation:
    """A user's machine with rookery: its own HOME, XDG directories and SSH key, and
    the `rookery serve` its commands use, on a database of its own, on its socket."""

    def __init__(self, name, ssh_key=True, source_dir=None, trust=True):
        self.root = SCRATCH / name
        self.home = self.root / 'home'
        self.config = self.home / '.config'
        self.data = self.home / '.local/share'
        self.run_dir = runtime_dir()
        _runtime_dirs.append(self.run_dir)
        for path in (self.home / '.ssh', self.config, self.data):
            path.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.env = {
            'HOME': str(self.home), 'USER': 'tester', 'LOGNAME': 'tester',
            'XDG_CONFIG_HOME': str(self.config), 'XDG_DATA_HOME': str(self.data),
            'XDG_RUNTIME_DIR': str(self.run_dir),
            # Its own server's socket, whoever runs the tests.
            'ROOKERY_SOCKET': str(self.run_dir / 'rookery/server.sock'),
            'PATH': os.environ['PATH'], 'LANG': 'C.UTF-8', 'TERM': 'xterm-256color',
            'ROOKERY_SOURCE': str(source_dir or source()),
            'ANSIBLE_LOCAL_TEMP': str(self.root / 'ansible-tmp'), 'ANSIBLE_FORCE_COLOR': '0',
            'ANSIBLE_CALLBACK_PLUGINS': str(Path(__file__).resolve().parent / 'fixtures/callbacks'),
            'ANSIBLE_CALLBACKS_ENABLED': 'e2e_applies', 'ROOKERY_E2E_APPLIES': str(APPLIES),
        }
        MADE['workstation'] += 1
        self.database = make_database(name)
        secret_key = self.root / 'secret.key'
        secret_key.write_text(secrets.token_hex(32) + '\n')
        secret_key.chmod(0o600)
        self.port = free_port()
        self.env.update({
            'ROOKERY_DATABASE_URL': self.database, 'ROOKERY_SECRET_KEY_FILE': str(secret_key),
            'ROOKERY_LISTEN': f'127.0.0.1:{self.port}', 'ROOKERY_PUBLIC_URL': f'http://127.0.0.1:{self.port}',
        })
        self.url = f'http://127.0.0.1:{self.port}'
        self.server = None
        self.start_server()
        self.key = self.home / '.ssh/id_ed25519'
        self.public = keygen(self.key, f'tester@{name}') if ssh_key else None
        # A host that is not up yet is trusted once it answers.
        if trust:
            self.trust_host()

    def start_server(self, env=None):
        """Start this machine's `rookery serve` and wait until its hosts are ready."""
        log = open(self.root / 'serve.log', 'a')
        self.server = subprocess.Popen([str(ROOKERY), 'serve'], env={**self.env, **(env or {})}, cwd=self.home,
                                       stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        _servers.append(self)
        def ready():
            if self.server.poll() is not None:
                raise AssertionError(f'rookery serve ended:\n{(self.root / "serve.log").read_text()[-4000:]}')
            done = subprocess.run([str(ROOKERY), 'host', 'list'], env=self.env, cwd=self.home,
                                  capture_output=True, text=True, timeout=30)
            return done.returncode == 0
        wait_for('rookery serve ready', ready, timeout=60)

    def stop_server(self):
        if self.server and self.server.poll() is None:
            self.server.send_signal(signal.SIGTERM)
            try:
                self.server.wait(20)
            except subprocess.TimeoutExpired:
                self.server.kill()
                self.server.wait()
        self.server = None

    @property
    def rookery_dir(self):
        return self.config / 'rookery'

    @property
    def own_key(self):
        """The file of rookery's own key, the key `rookery` of its key ring, which
        rookery writes from its store when ssh needs it."""
        return self.data / 'rookery/keys/rookery'

    def own_public(self):
        """The public half of rookery's own key, made when first asked for."""
        return self.rookery('key', 'show').stdout.strip()

    def own_key_file(self):
        """rookery's own key as a file: rookery writes it for an inventory, here
        of a host that is only made for that and removed again."""
        probe = 'key-file'
        self.write_profile({'hostname': probe, 'builder': {}, 'access': {'address': '192.0.2.1'}}, name=probe)
        self.rookery('host', 'inventory', probe)
        self.remove_host(probe)
        return self.own_key

    def trust_host(self):
        """Know the test host's key, as after a first ssh with a check of its fingerprint."""
        scanned = wait_for('the test host answering on SSH', lambda: subprocess.run(
            ['ssh-keyscan', '-T', '2', '-t', 'ed25519', HOST], capture_output=True, text=True, check=True
        ).stdout.strip(), timeout=60)
        (self.home / '.ssh/known_hosts').write_text(scanned + '\n')
        # ssh finds ~ in the password database, not in HOME: the runner's
        # own list of known hosts is the one it reads. The tests of another
        # host may run at the same time and keep theirs in it.
        known = Path('/etc/ssh/ssh_known_hosts')
        with open(f'{known}.lock', 'w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            others = [line for line in (known.read_text().splitlines() if known.exists() else [])
                      if line.split(' ', 1)[0] != HOST]
            known.with_suffix('.new').write_text(''.join(line + '\n' for line in [*others, scanned]))
            known.with_suffix('.new').replace(known)

    def rookery(self, *args, input=None, check=True, timeout=600, env=None):
        done = subprocess.run([str(ROOKERY), *args], env={**self.env, **(env or {})}, cwd=self.home,
                              input=input, capture_output=True, text=True, timeout=timeout)
        # What a job wrote goes to standard error, and the result to standard
        # output; the tests of a command that follows a job read both.
        done.output = done.stdout + done.stderr
        if follows_a_job(args):
            done.stdout = done.output
        if check and done.returncode != 0:
            raise AssertionError(f'rookery {" ".join(args)} exited {done.returncode}:\n'
                                 f'{done.stdout[-4000:]}\n{done.stderr[-4000:]}')
        return done

    def spawn(self, *args, env=None):
        return subprocess.Popen([str(ROOKERY), *args], env={**self.env, **(env or {})}, cwd=self.home,
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                text=True)

    def ssh(self, user, command, key=None, check=True, input=None, timeout=60):
        """A command on the test host over plain SSH, with one key only."""
        argv = ['ssh', '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'ConnectTimeout=10',
                '-o', f'UserKnownHostsFile={self.home}/.ssh/known_hosts',
                '-i', str(key or self.key), f'{user}@{HOST}', command]
        done = subprocess.run(argv, env=self.env, input=input, capture_output=True, text=True, timeout=timeout)
        if check and done.returncode != 0:
            raise AssertionError(f'ssh {user}@{HOST} {command!r} exited {done.returncode}: {done.stderr}')
        return done

    def root_on_host(self, command, check=True):
        return self.ssh(OPERATOR, f'sudo -n sh -c {shell_quote(command)}', check=check)

    def write_profile(self, profile, name=HOST):
        """Save a host's profile in rookery's store, as `rookery host import` does."""
        self.rookery('host', 'import', name, '-', input=json.dumps(profile, indent=2) + '\n')

    def read_profile(self, name=HOST):
        return json.loads(self.rookery('host', 'export', name).stdout)

    def remove_host(self, name=HOST, check=True):
        """Forget a host, with what rookery keeps of it."""
        return self.rookery('host', 'remove', name, '-y', check=check)

    def link(self):
        return RookerydLink(self)

    def terminal(self, *args, cols=100, rows=30):
        return Terminal([str(ROOKERY), *args], self.env, cols, rows, self.home)


_runtime_dirs = []


def fresh_runtime_dir():
    """A runtime directory of its own, so no shared SSH connection is used again."""
    path = runtime_dir()
    _runtime_dirs.append(path)
    return str(path)


def shell_quote(text):
    return "'" + text.replace("'", "'\"'\"'") + "'"


def profile(**values):
    """A profile like rookery writes for a new host, with the test host's address."""
    base = json.loads((SRC / 'provision/config/example.json').read_text())
    base.update({
        'hostname': HOST, 'target': 'cloud', 'keyboard': 'us',
        # The operator's sudo needs no password, as by default, so rookery asks
        # for none. shipped_plugins says the features are chosen already, as
        # in every profile rookery saves. rookery logs in with the user's SSH
        # configuration, which offers the user's key before rookery's own: the
        # host takes the user's key until the first apply gives it rookery's.
        'builder': {'shipped_plugins': True},
        'access': {'address': HOST, 'user': OPERATOR, 'auth': 'ssh'},
        'ansible_vars': light_vars(),
    })
    base.update(values)
    return base


def light_vars(features=()):
    """What keeps the role to the tasks the tests apply: no services to start
    (there is no systemd), no GPU, no plugins unless asked for."""
    return {
        'rookery_plugin_features': list(features),
        'rookery_start_services': False,
        'rookery_software_rendering': False,
        'rookery_selected_commands': [],
        'rookery_enabled_categories': [],
        'rookery_install_docker': False,
        'rookery_install_tailscale': False,
        'rookery_install_cockpit': False,
        'rookery_install_playwright': False,
    }


# ---------------------------------------------------------------------------
# The test host


def hand_over_operator_key(*public):
    """Put keys in place for the operator, as a cloud provider does at the start."""
    SEED.mkdir(parents=True, exist_ok=True)
    (SEED / f'{OPERATOR}.pub').write_text(''.join(key + '\n' for key in public))


_provisioned = {}


def provisioned():
    """The workstation that set the test host up, applying its profile once per run."""
    if 'workstation' in _provisioned:
        return _provisioned['workstation']
    station = Workstation('workstation')
    hand_over_operator_key(station.public)
    wait_for('the operator taking the provided key', lambda: station.ssh(OPERATOR, 'true', check=False).returncode == 0)
    extra = keygen(station.root / 'second_key', 'second@workstation')
    agent_one = keygen(station.root / 'agent_one', 'agent-one@workstation')
    agent_two = keygen(station.root / 'agent_two', 'agent-two@workstation')
    station.write_profile(profile(
        operator_ssh_public_keys=[station.public, extra],
        agent_ssh_public_keys=[agent_one, agent_two],
    ))
    started = time.monotonic()
    first = station.rookery('apply', HOST, timeout=900)
    station.apply_seconds = time.monotonic() - started
    station.first_apply = first.output
    wait_for('rookeryd answering', lambda: station.link().get('/v1/version'), timeout=60)
    _provisioned['workstation'] = station
    return station


def adopt(station):
    """Take a workstation whose host was set up otherwise, such as by its
    first boot, as the one provisioned() gives the tests."""
    _provisioned['workstation'] = station


def served():
    """The rookery serve of a workstation of its own, reached over TCP with an admin's
    token, set up through its API to reach the test host; made once per run."""
    if 'serve' in _provisioned:
        return _provisioned['serve']
    station = provisioned()
    server = Workstation('server', ssh_key=False)
    own = server.own_public()
    # The server's key reaches the operator the way the first one did.
    hand_over_operator_key(station.public, own)
    key = server.own_key_file()
    wait_for('the operator taking the server\'s key', lambda: server.ssh(
        OPERATOR, 'true', key=key, check=False).returncode == 0)
    # The registry is a folder of the server's machine, which a caller there
    # adds, not one over TCP.
    server.rookery('plugin', 'registry', 'add', 'git', plugin_registry().url)
    serve = Serve(server)
    # The host's profile as the workstation has it, so applying it from the
    # server keeps the workstation's keys; the server adds its own. What the
    # workstation turned on for its own tests stays there.
    host = station.read_profile()
    host['operator_ssh_public_keys'].append(station.own_public())
    # The server logs in with its own key; the SSH configuration is its own account's.
    host['access']['auth'] = 'rookery'
    host['ansible_vars'] = light_vars()
    for key in ('secrets', 'registries'):
        host.pop(key, None)
    serve.call('hosts.save', {'host': HOST, 'profile': host})
    _provisioned['serve'] = serve
    return serve


def stop_serving():
    serve = _provisioned.pop('serve', None)
    if serve:
        serve.station.stop_server()
        hand_over_operator_key(_provisioned['workstation'].public)


# ---------------------------------------------------------------------------
# States of the test host that several test cases share. Each is made with
# one apply (and one more to show that applying again changes nothing), the
# first time a test case asks for it, and its tests then only look. The host
# is in one of them at a time: asking for another first undoes what the
# current one set up on the workstation, the server and the tailnet, and
# then applies its own. run.py orders the test cases so each state is made
# once in a full run; a test case run alone makes the one it needs.

FIXTURE = SRC / 'tests/e2e/fixtures/plugin'
# The features of the extended state, from three kinds of plugin: a folder
# added to rookery, a package from a registry, and one that comes with rookery.
EXTENDED_FEATURES = ['e2e/hello', 'up/main', 'shipit/main']

_state = {'name': None, 'value': None, 'leave': None}


def state(name, enter):
    """The shared state `name`, made by `enter` unless it is the current one.
    `enter` returns (value, leave): what the tests get, and what undoes it."""
    if _state['name'] == name:
        return _state['value']
    leave, _state['name'], _state['leave'] = _state['leave'], None, None
    if leave:
        leave()
    value, leave = enter()
    _state.update(name=name, value=value, leave=leave)
    return value


def package_kdl(plugin, version, name, group='example'):
    """The package.kdl of a plugin package for Rookery."""
    return f'''format 1
package {{
  group {group}
  id {plugin}
  version "{version}"
}}
about {{
  name "{name}"
}}
targets {{
  rookery {{
    requires {{
      rookery ">=0.6"
    }}
  }}
}}
'''


def plugin_package(folder, plugin, manifest, files=None, name=None):
    """A plugin package in `folder`: its package.kdl, rookery.json and other
    files of its target, as {path: text}. Returns its target folder."""
    target = folder / 'rookery'
    target.mkdir(parents=True)
    (folder / 'package.kdl').write_text(package_kdl(plugin, '1.0.0', name or plugin.title()))
    (target / 'rookery.json').write_text(json.dumps(manifest, indent=2))
    for path, text in (files or {}).items():
        (target / path).parent.mkdir(parents=True, exist_ok=True)
        (target / path).write_text(text)
    return target


def plugin_version(version, plugin='up', group='example'):
    """The fixture plugin as GROUP/`plugin` at a version. From 2.0.0 its
    greeting and service changed, its Ansible task installs another file,
    the task `old` is gone and `added` is new."""
    folder = SCRATCH / f'{plugin}-{version}'
    if folder.exists():
        shutil.rmtree(folder)
    shutil.copytree(FIXTURE, folder)
    (folder / 'package.kdl').write_text(package_kdl(plugin, version, plugin.title(), group))
    manifest = json.loads((folder / 'rookery/rookery.json').read_text())
    feature = manifest['features'][0]
    feature['id'] = 'main'
    del feature['guide']
    newer = version != '1.0.0'
    feature['actions'] = [
        {'name': 'hello', 'label': 'Say hello', 'run': f'echo "hello from {version}"'},
        {'name': 'added' if newer else 'old', 'label': 'Added' if newer else 'Old',
         'run': f'echo "{"added" if newer else "old"} task"'},
    ]
    feature['services'][0]['run'] = f'exec box serve {plugin}/echo -- /usr/local/bin/rookery-e2e-echo {version}'
    feature['channels'] = [{'topic': 'notes', 'keep': 'log', 'max': 50}]
    (folder / 'rookery/rookery.json').write_text(json.dumps(manifest, indent=2))
    (folder / 'rookery/tasks/install.yml').write_text(f'''---
- name: Create the folder of what the plugin brings
  ansible.builtin.file:
    path: /usr/local/lib/rookery-{plugin}
    state: directory
    mode: '0755'
- name: Install what this version of the plugin brings
  ansible.builtin.copy:
    dest: /usr/local/lib/rookery-{plugin}/{"two" if newer else "one"}
    content: "{version}\\n"
    mode: '0644'
''')
    return folder


def joining(profile, **values):
    """A profile whose host gets Tailscale and joins the tailnet."""
    profile = {**profile, **values}
    profile['ansible_vars'] = {**profile.get('ansible_vars', {}), 'rookery_install_tailscale': True}
    return profile


def leave_the_tailnet(station):
    """Start the host's tailscaled afresh, as a machine that never joined, and
    have Headscale forget it."""
    station.root_on_host('rm -rf /var/lib/tailscale; pkill -x tailscaled', check=False)
    wait_for('tailscaled answering again', lambda: station.root_on_host(
        'tailscale status --json', check=False).stdout.strip())
    headscale().forget_nodes()


def last_key_id():
    """The newest of Headscale's keys."""
    return max([int(key['id']) for key in headscale().keys()], default=0)


class Applied:
    """What a shared state was made with: its applies and what was asked before and after."""

    def __init__(self, **values):
        self.__dict__.update(values)


def extended():
    """The host applied from the workstation with three plugins' features
    (EXTENDED_FEATURES) and joined to the tailnet with a key rookery made, then
    applied again. `first` and `second` are the two applies; `before` and
    `after_first` the newest Headscale key before each."""
    return state('extended', _enter_extended)


def _enter_extended():
    station = provisioned()
    kept = station.read_profile()
    registry = plugin_registry()
    # rookery's source with a plugin that comes with it, for the rest of the run.
    station.env['ROOKERY_SOURCE'] = str(SCRATCH / 'source-shipped' if (SCRATCH / 'source-shipped').exists()
                                      else source_with_plugins('source-shipped', {
                                          'shipit': plugin_version('1.0.0', 'shipit', 'rookery')}))
    # The apply runs in the server, which reads its source when it starts.
    station.stop_server()
    station.start_server()
    station.rookery('plugin', 'add', str(FIXTURE))
    registry.publish(plugin_version('1.0.0'))
    station.rookery('plugin', 'registry', 'add', 'git', registry.url)
    station.rookery('plugin', 'add', 'example/up')
    leave_the_tailnet(station)
    station.rookery('config', 'set', 'tailnet_login_server', headscale().url)
    station.rookery('config', 'set', 'tailnet_user', headscale().user)
    station.rookery('secret', 'set', 'tailnet_api_key', input=headscale().api_key + '\n')
    station.write_profile(joining({**kept, 'ansible_vars': light_vars(EXTENDED_FEATURES)}))
    before = last_key_id()
    first = station.rookery('apply', HOST, timeout=900)
    after_first = last_key_id()
    second = station.rookery('apply', HOST, timeout=900)

    def leave():
        leave_the_tailnet(station)
        station.write_profile(kept)
        for key in ('tailnet_login_server', 'tailnet_user'):
            station.rookery('config', 'unset', key)
        station.rookery('secret', 'remove', 'tailnet_api_key', '-y', check=False)
        for package in ('example/e2e', 'example/up'):
            station.rookery('plugin', 'remove', package, '-y', check=False)
    return Applied(station=station, kept=kept, first=first, second=second, before=before,
                   after_first=after_first), leave


def plain():
    """The host applied from the workstation again without the extended
    state's plugins: joined to the tailnet by hand, and no API key of the
    tailnet on the workstation. `first` is that apply,
    `nodes` and `after` Headscale's nodes before and after it, and `before`
    the newest Headscale key after the host joined. Then the host leaves the tailnet, and `refused` is
    the apply that finds it off the tailnet with no way to a key."""
    return state('plain', _enter_plain)


def join_by_hand(station):
    """Join the host to Headscale with a key made by hand."""
    key = headscale().new_key()
    station.root_on_host(f"umask 077; printf %s {shell_quote(key)} > /run/e2e-key; "
                         f"tailscale up --timeout=60s --auth-key=file:/run/e2e-key "
                         f"--login-server={shell_quote(headscale().url)}; status=$?; rm -f /run/e2e-key; exit $status")


def _enter_plain():
    station = provisioned()
    for package in ('example/e2e', 'example/up'):
        if (station.rookery_dir / 'plugins' / package).exists():
            station.rookery('plugin', 'remove', package, '-y')
    kept = station.read_profile()
    leave_the_tailnet(station)
    join_by_hand(station)
    station.rookery('config', 'set', 'tailnet_login_server', headscale().url)
    station.rookery('config', 'set', 'tailnet_user', headscale().user)
    station.write_profile(joining({**kept, 'ansible_vars': light_vars()}))
    nodes, before = headscale().nodes(), last_key_id()
    first = station.rookery('apply', HOST, timeout=900)
    after = headscale().nodes()
    leave_the_tailnet(station)
    refused = station.rookery('apply', HOST, timeout=900, check=False)

    def leave():
        leave_the_tailnet(station)
        station.write_profile(kept)
        for key in ('tailnet_login_server', 'tailnet_user'):
            station.rookery('config', 'unset', key)
    return Applied(station=station, kept=kept, first=first, nodes=nodes, after=after,
                   before=before, refused=refused), leave


def served_extended():
    """The host applied by rookery serve with a plugin from the registry and
    joined to the tailnet with a key the server made: first by a terminal
    client of the server, then again as a job of the server, followed over
    /ws. `client` is that client; `first` its apply; `job` the job's id, `texts`
    the lines following it gave (its log items, then how it ended) and `data`
    the text of its log; `before`, `after_first`
    and `after_second` the newest Headscale key before and after each."""
    return state('served-extended', _enter_served_extended)


def _enter_served_extended():
    serve = served()
    station = provisioned()
    leave_the_tailnet(station)
    registry = plugin_registry()
    registry.publish(plugin_version('1.0.0', 'served'))
    serve.call('plugins.install', {'spec': 'example/served'})
    client = Workstation('served-client', ssh_key=False)
    client.rookery('login', serve.url, '--token-stdin', input=serve.token)
    client.rookery('config', 'set', 'tailnet_login_server', headscale().url)
    client.rookery('config', 'set', 'tailnet_user', headscale().user)
    client.rookery('secret', 'set', 'tailnet_api_key', input=headscale().api_key + '\n')
    kept = serve.call('hosts.profile', {'host': HOST})['profile']
    serve.call('hosts.save', {'host': HOST, 'profile': joining({**kept, 'ansible_vars': light_vars(['served/main'])})})
    before = last_key_id()
    first = client.rookery('apply', HOST, timeout=900)
    after_first = last_key_id()
    job = serve.call('hosts.act', {'host': HOST, 'action': 'apply'})['job']
    socket = serve.socket()
    try:
        # The stream ends by itself after the line with how the job ended.
        raw, _ = socket.stream('jobs.follow', {'id': job, 'after': 0}, timeout=900)
    finally:
        socket.close()
    texts = [json.loads(line) for line in raw.decode().splitlines() if line.strip()]
    data = '\n'.join(text['text'] for text in texts if 'seq' in text)
    after_second = last_key_id()

    def leave():
        leave_the_tailnet(station)
        serve.call('hosts.save', {'host': HOST, 'profile': kept})
        serve.rpc('plugins.remove', {'id': 'served'})
        for key in ('tailnet_login_server', 'tailnet_user'):
            serve.call('installation.settings.set', {'name': key, 'value': ''})
        serve.rpc('installation.secrets.remove', {'name': 'tailnet_api_key'})
    return Applied(serve=serve, station=station, client=client, kept=kept, first=first, job=job, texts=texts,
                   data=data, before=before, after_first=after_first, after_second=after_second), leave


def ansible_recap(output):
    """ok, changed and failed of the one host in an Ansible recap."""
    found = re.search(r'ok=(\d+)\s+changed=(\d+)\s+unreachable=(\d+)\s+failed=(\d+)', output)
    if not found:
        raise AssertionError(f'No play recap in:\n{output[-3000:]}')
    ok, changed, unreachable, failed = map(int, found.groups())
    return {'ok': ok, 'changed': changed, 'unreachable': unreachable, 'failed': failed}


def changed_tasks(output):
    """The names of the tasks Ansible reported as changed."""
    names, current = [], None
    for line in output.splitlines():
        task = re.match(r'TASK \[(.*)\]', line)
        if task:
            current = task.group(1)
        elif line.startswith('changed:') and current and current not in names:
            names.append(current)
    return names


# ---------------------------------------------------------------------------
# rookeryd over the link rookery uses: HTTP on the standard input and output of
# `sudo rookeryd connect`, over SSH as the operator


class Pipe:
    def __init__(self, stream):
        self.stream = stream

    def makefile(self, *_args, **_kwargs):
        return self.stream


class RookerydLink:
    def __init__(self, station, key=None):
        self.station = station
        # rookery writes its key as a file only when something needs it.
        self.key = key or station.own_key_file()

    def request(self, method, path, body=None, raw=False, headers=None):
        """(status, answer) of one request; the answer is JSON unless raw."""
        data = b'' if body is None else body if isinstance(body, bytes) else json.dumps(body).encode()
        head = f'{method} {path} HTTP/1.1\r\nHost: rookery\r\nConnection: close\r\nContent-Length: {len(data)}\r\n'
        if not isinstance(body, bytes):
            head += 'Content-Type: application/json\r\n'
        for name, value in (headers or {}).items():
            head += f'{name}: {value}\r\n'
        argv = ['ssh', '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-i', str(self.key),
                '-o', f'UserKnownHostsFile={self.station.home}/.ssh/known_hosts',
                '-T', f'{OPERATOR}@{HOST}', 'sudo -n rookeryd connect']
        link = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                env=self.station.env)
        try:
            link.stdin.write(head.encode() + b'\r\n' + data)
            link.stdin.flush()
            answer = http.client.HTTPResponse(Pipe(link.stdout), method=method)
            answer.begin()
            text = answer.read()
            return answer.status, text if raw else json.loads(text or b'null')
        except (http.client.HTTPException, ConnectionError) as error:
            said = link.stderr.read().decode(errors='replace') if link.poll() is not None else ''
            raise AssertionError(f'{method} {path}: {said or error!r}') from error
        finally:
            link.kill()
            link.wait()

    def get(self, path):
        status, answer = self.request('GET', path)
        if status >= 400:
            raise AssertionError(f'GET {path}: {status} {answer}')
        return answer

    def post(self, path, body=None):
        status, answer = self.request('POST', path, body if body is not None else {})
        if status >= 400:
            raise AssertionError(f'POST {path}: {status} {answer}')
        return answer


def b64(data):
    return base64.b64encode(data if isinstance(data, bytes) else data.encode()).decode()


def unb64(text):
    return base64.b64decode(text or '').decode(errors='replace')


# ---------------------------------------------------------------------------
# rookery serve


class Refused(dict):
    """The error of a call, as {'error': …}: apart from a result that has an `error` of its own,
    as a job's has."""


class Serve:
    """The `rookery serve` of a workstation as a client elsewhere reaches it: over
    TCP, with an access token of a person, `ada`, who is an admin."""

    def __init__(self, station):
        MADE['serve'] += 1
        self.station = station
        self.port = station.port
        self.url = station.url
        self.token = self.make_token('ada', admin=True)

    def make_token(self, user, admin=False):
        """An access token of `user`, made on the server's own machine; the person is made too."""
        args = ['server', 'token', '--user', user, '--name', f'e2e-{secrets.token_hex(3)}']
        if admin:
            args.append('--admin')
        return self.station.rookery(*args).stdout.strip().splitlines()[0]

    def stop(self):
        """The server keeps running with its workstation; nothing of its own stops."""

    def rpc(self, method, params=None, token=None):
        """(HTTP status, the answer: its result, or its error) of one call over POST /rpc."""
        connection = http.client.HTTPConnection('127.0.0.1', self.port, timeout=300)
        bearer = self.token if token is None else token
        headers = {'Content-Type': 'application/json'}
        if bearer:
            headers['Authorization'] = f'Bearer {bearer}'
        body = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params or {}})
        try:
            connection.request('POST', '/rpc', body=body, headers=headers)
            answer = connection.getresponse()
            text = answer.read()
            said = json.loads(text or b'null')
        finally:
            connection.close()
        if isinstance(said, dict) and 'error' in said:
            return answer.status, Refused({'error': said['error']})
        return answer.status, said.get('result') if isinstance(said, dict) else said

    def call(self, method, params=None, token=None):
        """The result of a method, or an AssertionError with its error."""
        status, answer = self.rpc(method, params, token)
        if isinstance(answer, Refused):
            raise AssertionError(f'{method}: {status} {answer["error"]}')
        return answer

    def refused(self, method, params=None, token=None):
        """The error code a method is refused with, such as FORBIDDEN, or None when it is not."""
        _, answer = self.rpc(method, params, token)
        if isinstance(answer, Refused):
            return answer['error'].get('data', {}).get('code')
        return None

    def socket(self, token=None):
        return RpcSocket(self.port, self.token if token is None else token)

    def wait_job(self, job, timeout=900):
        """The job once it has ended, and its log as text."""
        wait_for(f'the job {job} ending', lambda: self.call('jobs.get', {'id': job})['state']
                 in ('succeeded', 'failed', 'cancelled'), timeout=timeout, every=1)
        return self.call('jobs.get', {'id': job}), self.log(job)

    def log(self, job):
        lines, after = [], 0
        while True:
            items = self.call('jobs.output', {'id': job, 'after': after})['items']
            lines += [item['text'] for item in items]
            if len(items) < 1000:
                return '\n'.join(lines)
            after = items[-1]['seq']


def free_port():
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        return probe.getsockname()[1]


# ---------------------------------------------------------------------------
# A WebSocket client, enough for /ws


class WebSocket:
    def __init__(self, port, path, protocols=(), headers=None, timeout=60):
        self.sock = socket.create_connection(('127.0.0.1', port), timeout=timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        lines = [f'GET {path} HTTP/1.1', f'Host: 127.0.0.1:{port}', 'Upgrade: websocket',
                 'Connection: Upgrade', f'Sec-WebSocket-Key: {key}', 'Sec-WebSocket-Version: 13']
        if protocols:
            lines.append('Sec-WebSocket-Protocol: ' + ', '.join(protocols))
        lines += [f'{name}: {value}' for name, value in (headers or {}).items()]
        self.sock.sendall(('\r\n'.join(lines) + '\r\n\r\n').encode())
        head = b''
        while b'\r\n\r\n' not in head:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError('closed during the handshake')
            head += chunk
        head, self.buffer = head.split(b'\r\n\r\n', 1)
        lines = head.decode().split('\r\n')
        self.status = int(lines[0].split()[1])
        self.headers = {name.lower(): value.strip() for name, _, value in
                        (line.partition(':') for line in lines[1:])}
        if self.status != 101:
            body = self.buffer.decode(errors='replace')
            self.sock.close()
            raise PermissionError(f'{self.status} {body}')
        accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
        assert self.headers.get('sec-websocket-accept') == accept.decode()

    def _read(self, count):
        while len(self.buffer) < count:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError('closed')
            self.buffer += chunk
        data, self.buffer = self.buffer[:count], self.buffer[count:]
        return data

    def send(self, payload, opcode=None):
        if isinstance(payload, (dict, list)):
            payload = json.dumps(payload)
        if isinstance(payload, str):
            opcode, payload = opcode or 1, payload.encode()
        else:
            opcode = opcode or 2
        mask = os.urandom(4)
        length = len(payload)
        if length < 126:
            head = struct.pack('!BB', 0x80 | opcode, 0x80 | length)
        elif length < 65536:
            head = struct.pack('!BBH', 0x80 | opcode, 0x80 | 126, length)
        else:
            head = struct.pack('!BBQ', 0x80 | opcode, 0x80 | 127, length)
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self.sock.sendall(head + mask + masked)

    def recv(self):
        """The next text (as JSON) or binary message; None once closed."""
        message, kind = b'', None
        while True:
            first, second = self._read(2)
            opcode, length = first & 0x0F, second & 0x7F
            if length == 126:
                length = struct.unpack('!H', self._read(2))[0]
            elif length == 127:
                length = struct.unpack('!Q', self._read(8))[0]
            payload = self._read(length)
            if opcode == 8:
                return None
            if opcode == 9:
                self.send(payload, opcode=10)
                continue
            if opcode == 10:
                continue
            if opcode in (1, 2):
                kind = opcode
            message += payload
            if first & 0x80:
                return json.loads(message) if kind == 1 else message

    def close(self):
        with contextlib.suppress(OSError):
            self.send(b'', opcode=8)
        self.sock.close()


class RpcSocket:
    """JSON-RPC over /ws with its stream channels: calls, and the bytes of a stream."""

    def __init__(self, port, token):
        headers = {'Authorization': f'Bearer {token}'} if token else {}
        self.ws = WebSocket(port, '/ws', headers=headers)
        self.next = 1
        self.pending = []

    def _take(self, matches, timeout):
        for frame in list(self.pending):
            if matches(frame):
                self.pending.remove(frame)
                return frame
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.ws.sock.settimeout(max(0.1, end - time.monotonic()))
            frame = self.ws.recv()
            if frame is None:
                raise ConnectionError('the connection closed')
            if matches(frame):
                return frame
            self.pending.append(frame)
        raise TimeoutError('no answer on /ws')

    def call(self, method, params=None, timeout=120):
        """The whole answer: result or error."""
        number = self.next
        self.next += 1
        self.ws.send({'jsonrpc': '2.0', 'id': number, 'method': method, 'params': params or {}})
        return self._take(lambda got: isinstance(got, dict) and got.get('id') == number, timeout)

    def result(self, method, params=None, timeout=120):
        answer = self.call(method, params, timeout)
        if 'error' in answer:
            raise AssertionError(f'{method}: {answer["error"]}')
        return answer['result']

    def send(self, stream, data):
        self.ws.send(struct.pack('!I', stream) + data)

    def stream(self, method, params=None, until=None, timeout=120, send=None):
        """Open a stream and collect its bytes until it ends or `until(data)` is true. `send`
        is written to it first."""
        stream = self.result(method, params, timeout=timeout)['stream']
        if send is not None:
            self.send(stream, send)
        tag = struct.pack('!I', stream)
        data = b''

        def mine(frame):
            if isinstance(frame, bytes):
                return frame[:4] == tag
            return (frame.get('method', '').startswith('streams.')
                    and frame.get('params', {}).get('stream') == stream)

        end = time.monotonic() + timeout
        while time.monotonic() < end:
            frame = self._take(mine, end - time.monotonic())
            if isinstance(frame, bytes):
                data += frame[4:]
                self.ws.send({'jsonrpc': '2.0', 'method': 'streams.credit',
                              'params': {'stream': stream, 'bytes': len(frame) - 4}})
            elif frame['method'] == 'streams.close':
                return data, frame['params'].get('error')
            elif frame['method'] == 'streams.eof':
                return data, None
            if until and until(data):
                self.ws.send({'jsonrpc': '2.0', 'method': 'streams.close', 'params': {'stream': stream}})
                return data, None
        raise TimeoutError(f'{method} did not end')

    def through(self, method, params, data, timeout=60):
        """What comes back through a byte stream, such as a port's, for `data` and this side's
        end; the answer's error when it does not open."""
        answer = self.call(method, params, timeout=timeout)
        if 'error' in answer:
            return answer
        stream = answer['result']['stream']
        tag = struct.pack('!I', stream)
        credit, sent, back = 256 * 1024, 0, b''
        end = time.monotonic() + timeout

        def take(frame):
            nonlocal credit, back
            if isinstance(frame, bytes) and frame[:4] == tag:
                back += frame[4:]
                self.ws.send({'jsonrpc': '2.0', 'method': 'streams.credit',
                              'params': {'stream': stream, 'bytes': len(frame) - 4}})
            elif isinstance(frame, dict) and frame.get('params', {}).get('stream') == stream:
                if frame.get('method') == 'streams.credit':
                    credit += frame['params']['bytes']
                elif frame.get('method') in ('streams.eof', 'streams.close'):
                    return True
            return False

        while sent < len(data):
            chunk = data[sent:sent + min(32768, credit)]
            if chunk:
                self.send(stream, chunk)
                credit -= len(chunk)
                sent += len(chunk)
                continue
            self.ws.sock.settimeout(max(0.1, end - time.monotonic()))
            if take(self.ws.recv()):
                return back
        self.ws.send({'jsonrpc': '2.0', 'method': 'streams.eof', 'params': {'stream': stream}})
        while time.monotonic() < end:
            self.ws.sock.settimeout(max(0.1, end - time.monotonic()))
            if take(self.ws.recv()):
                return back
        raise TimeoutError(f'{method} did not end')

    def close(self):
        self.ws.close()


# ---------------------------------------------------------------------------
# An npm registry that answers /-/ping and /-/whoami for one login


class NpmRegistry:
    def __init__(self, user, password):
        expected = 'Basic ' + b64(f'{user}:{password}')
        self.asked = []
        registry = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):  # noqa: N802
                registry.asked.append(self.path)
                if self.headers.get('Authorization') != expected:
                    status, body = 401, {'error': 'login'}
                elif self.path.endswith('/-/whoami'):
                    status, body = 200, {'username': user}
                else:
                    status, body = 200, {}
                data = json.dumps(body).encode()
                self.send_response(status)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.url = f'http://127.0.0.1:{self.server.server_address[1]}/repository/npm/'
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def stop(self):
        self.server.shutdown()


# ---------------------------------------------------------------------------
# A plugin registry: a git repository with a folder for each package and a
# tag for each version, in a folder of this machine, so no version waits
# for a minimum age


class PluginRegistry:
    def __init__(self):
        self.dir = Path(tempfile.mkdtemp(prefix='registry-', dir=SCRATCH))
        self.url = str(self.dir)
        self.released = set()
        (self.dir / 'packages.kdl').write_text(
            'format 1\npackages {\n  find "*"\n}\nrelease {\n  tag "{id}-v{version}"\n}\n')
        self.git('init', '-q', '-b', 'main')
        self.git('add', '-A')
        self.git('commit', '-q', '-m', 'A registry of plugins')

    def git(self, *args):
        env = {**os.environ, 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull,
               'GIT_AUTHOR_NAME': 'e2e', 'GIT_AUTHOR_EMAIL': 'e2e@example.org',
               'GIT_COMMITTER_NAME': 'e2e', 'GIT_COMMITTER_EMAIL': 'e2e@example.org'}
        subprocess.run(['git', *args], cwd=self.dir, env=env, check=True, capture_output=True)

    def publish(self, folder):
        """Release a package folder: its files in the folder of its id, and the tag of its version."""
        manifest = (Path(folder) / 'package.kdl').read_text()
        plugin = re.search(r'^\s*id (\S+)', manifest, re.M).group(1)
        version = re.search(r'^\s*version "([^"]+)"', manifest, re.M).group(1)
        tag = f'{plugin}-v{version}'
        # A shared state entered again releases the same version again.
        if tag in self.released:
            return
        self.released.add(tag)
        shutil.rmtree(self.dir / plugin, ignore_errors=True)
        shutil.copytree(folder, self.dir / plugin)
        self.git('add', '-A')
        self.git('commit', '-q', '-m', f'{plugin} {version}')
        self.git('tag', tag)


_registry = []


def plugin_registry():
    """The plugin registry of this run, made once."""
    if not _registry:
        _registry.append(PluginRegistry())
    return _registry[0]


# ---------------------------------------------------------------------------
# The Headscale server of ci/compose.yml, which the test host can join


HEADSCALE = os.environ.get('ROOKERY_E2E_HEADSCALE', 'headscale')


class Headscale:
    """The `headscale` service of ci/compose.yml: its login server, the API key
    it hands out at start, and its API. It has one user, `rookery`, number 1."""

    def __init__(self):
        self.url = f'http://{HEADSCALE}:8080'
        self.user = 'rookery'

        def ready():
            with urllib.request.urlopen(f'http://{HEADSCALE}:8081/api-key', timeout=5) as answer:
                key = answer.read().decode().strip()
            # A key from an earlier start is refused until the new one is there.
            self.api_key = key
            return key and self.api('GET', '/api/v1/user') is not None
        wait_for('Headscale handing out its API key', ready, timeout=60)

    def api(self, method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.url + path, data=data, method=method, headers={
            'Authorization': f'Bearer {self.api_key}', 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=20) as answer:
            return json.loads(answer.read() or b'null')

    def nodes(self):
        return self.api('GET', '/api/v1/node').get('nodes', [])

    def keys(self):
        return self.api('GET', '/api/v1/preauthkey').get('preAuthKeys', [])

    def new_key(self, hours=1):
        """A one-use key of the user, as an admin makes one by hand."""
        expiry = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(time.time() + hours * 3600))
        made = self.api('POST', '/api/v1/preauthkey', {'user': '1', 'reusable': False, 'ephemeral': False,
                                                     'expiration': expiry})
        return made['preAuthKey']['key']

    def forget_nodes(self):
        for node in self.nodes():
            self.api('DELETE', f'/api/v1/node/{node["id"]}')


_headscale = []


def headscale():
    """The Headscale server, once it is ready."""
    if not _headscale:
        _headscale.append(Headscale())
    return _headscale[0]


# ---------------------------------------------------------------------------
# A pseudo-terminal, for the commands that want one


class Terminal:
    def __init__(self, argv, env, cols, rows, cwd):
        import fcntl  # noqa: PLC0415
        import termios  # noqa: PLC0415
        self.output = b''
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(cwd)
            os.execve(argv[0], argv, env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
        self.status = None

    def pump(self, seconds=0.2):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            ready, _, _ = select.select([self.fd], [], [], max(0.0, end - time.monotonic()))
            if not ready:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            if not data:
                return
            # Answer what crossterm asks the terminal, so it does not wait.
            if b'\x1b[6n' in data:
                os.write(self.fd, b'\x1b[1;1R')
            if b'\x1b[c' in data:
                os.write(self.fd, b'\x1b[?62;22c')
            self.output += data

    def text(self):
        plain = re.sub(rb'\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[()][A-Z0-9]|\x1b[=>]',
                       b'', self.output)
        return plain.decode(errors='replace')

    def expect(self, pattern, timeout=30):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if re.search(pattern, self.text()):
                return
            self.pump(0.2)
        raise AssertionError(f'{pattern!r} did not appear; the terminal showed:\n{self.text()[-3000:]}')

    def send(self, text):
        os.write(self.fd, text.encode() if isinstance(text, str) else text)

    def wait(self, timeout=30):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.pump(0.1)
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.status = os.waitstatus_to_exitcode(status)
                return self.status
        raise AssertionError(f'the program did not end; it showed:\n{self.text()[-3000:]}')

    def close(self):
        if self.status is None:
            with contextlib.suppress(ProcessLookupError):
                os.kill(self.pid, signal.SIGTERM)
            with contextlib.suppress(ChildProcessError):
                os.waitpid(self.pid, 0)
        with contextlib.suppress(OSError):
            os.close(self.fd)


def random_text(prefix):
    return f'{prefix}-{secrets.token_hex(6)}'


def cleanup():
    """Stop what runs, remove the scratch directories and the keys handed to the test host."""
    with contextlib.suppress(Exception):
        stop_serving()
    for station in _servers:
        with contextlib.suppress(Exception):
            station.stop_server()
    with contextlib.suppress(OSError):
        for key in SEED.glob('*.pub'):
            key.unlink()
    for path in [SCRATCH, *_runtime_dirs]:
        shutil.rmtree(path, ignore_errors=True)
