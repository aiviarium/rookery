"""Provisioners as subjects of their own, with the real rookery: a made-up
provisioner of a plugin in the workstation's own plugin folder, its page, its
settings and its actions, and a deployment it makes as a resource, from the
command line and through rookery serve, with the endpoints it reports and the
commands it runs. It needs no test host, and runs on `forge` beside the
modules there.

With ROOKERY_E2E_DOCKER_HOST set to a Docker engine the runner may use, such as
tcp://docker:2375, the Docker plugin's own actions are asked of it too, a
small Compose deployment is made, changed and removed there, and an instance
with a published port is reached through its endpoint and runs a command.
Without one those cases are skipped."""
import json
import os
import socket
import stat
import threading
import unittest

from harness import SCRATCH, Serve, Workstation, needs_e2e, plugin_package

# A provisioner that answers from what it is asked: it writes each request,
# secrets left out, to requests.jsonl of its folder.
PROGRAM = r'''#!/usr/bin/env python3
import json, sys, time
verb = sys.argv[1]
request = json.load(sys.stdin)
with open('requests.jsonl', 'a') as log:
    log.write(json.dumps({k: v for k, v in request.items() if k != 'secrets'}) + '\n')
if request.get('version') != 2:
    print('speaks version 2 only', file=sys.stderr)
    sys.exit(1)
place = request['arguments'].get('place') or 'nowhere'
if verb == 'status' and request.get('id'):
    print(json.dumps({'state': 'running'}))
elif verb == 'status':
    print(json.dumps({'message': f'The lab at {place} answers',
                      'details': [{'label': 'Benches', 'value': '3'}],
                      'values': {'shelf': ['top', 'bottom']},
                      'collections': {'shelves': [
                          {'name': 'top', 'columns': {'dust': 'little'}},
                          {'name': 'bottom', 'columns': {'dust': 'thick'},
                           'unavailable': {'tidy': 'The bottom shelf is out of reach'}}]}}))
elif verb == 'list':
    print(json.dumps({'resources': [{'id': 'box-1', 'kind': 'machine', 'name': 'attic', 'state': 'running'}]}))
elif verb in ('create', 'change') and request.get('kind') == 'deployment':
    # The secret reaches the program, which says it, so rookery masks it.
    print(f'deploying {request["name"]} with {request.get("environment", {}).get("TOKEN")}', file=sys.stderr)
    with open(f'spec.{request["name"]}', 'w') as spec:
        spec.write(request['spec'])
    answer = {'id': f'project-{request["name"]}', 'state': 'running'}
    # A spec that names a port of this machine is reached there.
    for line in request['spec'].splitlines():
        if line.startswith('listen: '):
            answer['endpoints'] = [{'name': 'bench', 'address': '127.0.0.1', 'port': int(line[8:]),
                                    'hint': 'echo', 'local': True}]
    print(json.dumps(answer))
elif verb == 'exec':
    print(f'ran {" ".join(request["command"])} reading {request.get("stdin", "nothing")!r}', file=sys.stderr)
    print(json.dumps({'exit_code': 4}))
elif verb == 'create' and request.get('kind') == 'machine':
    # A machine with no address: rookery keeps it, and cannot reach it to set it up.
    print(json.dumps({'id': f'box-{request["name"]}', 'state': 'running'}))
elif verb in ('start', 'stop'):
    print(json.dumps({'state': 'running' if verb == 'start' else 'stopped'}))
elif verb == 'destroy':
    print(json.dumps({'state': 'gone'}))
elif verb == 'logs':
    print(f'{request["id"]} started, tail {request["tail"]}')
elif verb == 'act':
    action = request['action']
    if request.get('id'):
        print(json.dumps({'message': f'{request["kind"]} {request["id"]} has 1 service',
                          'details': [{'label': 'web', 'value': 'running'}]}))
    elif action['name'] == 'tidy':
        print(json.dumps({'message': f'Tidied the {action["arguments"]["shelf"]} shelf'}))
    elif action['name'] == 'look':
        print(json.dumps({'message': f'Looked around {place}', 'details': [{'label': 'Dust', 'value': 'little'}]}))
    else:
        print(f'sweeping {action["arguments"].get("shelf")} with {action["arguments"].get("code")}', file=sys.stderr)
        time.sleep(0.5)
        print(json.dumps({'message': 'Swept'}))
else:
    print(f'no verb {verb}', file=sys.stderr)
    sys.exit(1)
'''

MANIFEST = {
    'provisioners': [{
        'id': 'box', 'label': 'Lab box', 'command': 'bin/provision', 'protocol': 2, 'verbs': ['list'],
        'arguments': [{'name': 'place', 'label': 'Place', 'required': True}],
        'kinds': [{'id': 'machine', 'setup': 'apply', 'verbs': ['start', 'stop']},
                  {'id': 'deployment', 'label': 'Bench setup', 'verbs': ['change', 'logs', 'exec'],
                   'spec': {'format': 'text', 'label': 'Setup'},
                   'actions': [{'name': 'services', 'label': 'Services', 'changes': False}]}],
        'actions': [
            {'name': 'look', 'label': 'Look around', 'changes': False},
            {'name': 'sweep', 'label': 'Sweep', 'confirm': 'Sweep the lab?',
             'arguments': [{'name': 'shelf', 'values': ['top', 'bottom'], 'default': 'top'},
                           {'name': 'code', 'secret': True}]},
            {'name': 'tidy', 'label': 'Tidy a shelf', 'group': 'Lab', 'arguments': [{'name': 'shelf'}]},
        ],
        'collections': [{'id': 'shelves', 'label': 'Shelf', 'plural': 'Shelves', 'key': ['shelf'],
                         'columns': [{'name': 'dust', 'label': 'Dust'}],
                         'item': [{'action': 'tidy', 'label': 'Tidy'}]}],
    }, {
        'id': 'old', 'label': 'Old box', 'command': 'bin/provision', 'setup': 'apply',
    }],
}


def lab(station):
    """The plugin added to the workstation's own plugins, registered as `box`
    and `old`, and the folder it was installed in."""
    package = SCRATCH / f'lab-{station.root.name}'
    target = plugin_package(package, 'lab', MANIFEST, {'bin/provision': PROGRAM})
    program = target / 'bin/provision'
    program.chmod(program.stat().st_mode | stat.S_IXUSR)
    station.rookery('plugin', 'add', str(package))
    folder = station.rookery_dir / 'plugins/example/lab/rookery'
    # rookery runs each type under a name of its own.
    station.rookery('provisioner', 'add', 'box', 'lab/box')
    station.rookery('provisioner', 'add', 'old', 'lab/old')
    return folder


def requests(folder):
    path = folder / 'requests.jsonl'
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


@needs_e2e
class Provisioners(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.station = Workstation('provisioners', ssh_key=False, trust=False)
        cls.folder = lab(cls.station)

    def rookery(self, *args, **kwargs):
        return self.station.rookery(*args, **kwargs)

    def test_a_provisioner_is_listed_with_why_it_cannot_be_used(self):
        listed = {found['name']: found for found in json.loads(self.rookery('provisioner', 'list', '-o', 'json').stdout)}
        self.assertEqual(listed['box']['type'], 'lab/box')
        self.assertEqual(listed['box']['problem'], 'box needs place: rookery provisioner set box place=VALUE')
        self.assertEqual(listed['old']['type'], 'lab/old')
        self.assertTrue(listed['old']['problem'].startswith('lab/old speaks version 1'), listed['old']['problem'])
        self.assertRegex(self.rookery('provisioner', 'list').stdout, r'(?m)^box\s+lab/box\s+no: box needs place: .*\s+Lab box$')
        types = self.rookery('provisioner', 'types').stdout
        self.assertRegex(types, r'(?m)^lab/box\s+Lab box\s+machine, deployment$')
        page = self.rookery('provisioner', 'show', 'old').stdout
        self.assertIn('unusable', page)

    def test_its_settings_page_and_actions_from_the_command_line(self):
        self.rookery('provisioner', 'run', 'box', 'settings', 'place=attic', '--yes')
        page = self.rookery('provisioner', 'show', 'box').stdout
        for line in ('State     ready · The lab at attic answers', 'Benches   3', 'Settings  place=attic',
                     'Machines rookery made with it', '  attic            box-1  running · rookery has no record of it',
                     '    look'):
            self.assertIn(line, page)
        # Its collection is a table, with what cannot be done to an item and why.
        for line in ('Shelves (2)\n', '  SHELF   DUST\n', '  top     little\n',
                     '  bottom: no tidy: The bottom shelf is out of reach\n', '  Shelves\n    tidy'):
            self.assertIn(line, page)
        actions = json.loads(self.rookery('provisioner', 'actions', 'box', '-o', 'json').stdout)
        self.assertEqual({action['name']: action.get('collection', '') for action in actions}['tidy'], 'shelves')
        self.assertRegex(self.rookery('provisioner', 'actions', 'box').stdout, r'(?m)^tidy\s+Lab\s+shelves\s+Tidy a shelf')
        tidied = self.rookery('provisioner', 'run', 'box', 'tidy', 'shelf=top', '--yes').stdout
        self.assertIn('Tidied the top shelf', tidied)
        # For this once, another place.
        self.assertIn('The lab at cellar answers', self.rookery('provisioner', 'show', 'box', 'place=cellar').stdout)
        looked = self.rookery('provisioner', 'run', 'box', 'look').stdout
        self.assertEqual(looked.splitlines(), ['Looked around attic', '  Dust  little'])
        swept = self.rookery('provisioner', 'run', 'box', 'sweep', 'shelf=bottom', '--yes')
        self.assertIn('sweeping bottom with \n', swept.stdout)
        self.assertIn('✓ Swept', swept.stdout)
        refused = self.rookery('provisioner', 'run', 'box', 'sweep', 'shelf=middle', '--yes', check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('shelf=top or bottom only', refused.stderr)
        self.assertEqual(requests(self.folder)[-1]['verb'], 'act')

    def test_through_rookery_serve_an_action_that_changes_something_is_a_job(self):
        self.rookery('provisioner', 'run', 'box', 'settings', 'place=attic', '--yes')
        serve = Serve(self.station)
        try:
            page = serve.call('provisioners.get', {'name': 'box'})
            self.assertEqual((page['state'], page['message']), ('ready', 'The lab at attic answers'))
            self.assertEqual(page['values'], {'shelf': ['top', 'bottom']})
            self.assertEqual([found['id'] for found in page['provisioner']['collections']], ['shelves'])
            self.assertEqual(page['collections']['shelves'][1],
                             {'name': 'bottom', 'key': {'shelf': 'bottom'}, 'columns': {'dust': 'thick'},
                              'unavailable': {'tidy': 'The bottom shelf is out of reach'}})
            actions = serve.call('provisioners.actions', {'name': 'box'})['actions']
            self.assertEqual([action['name'] for action in actions],
                             ['new-deployment', 'settings', 'look', 'sweep', 'tidy', 'adopt', 'rename', 'remove'])
            looked = serve.call('provisioners.act', {'name': 'box', 'action': 'look'})
            self.assertEqual(looked['result']['message'], 'Looked around attic')
            acted = serve.call('provisioners.act', {'name': 'box', 'action': 'sweep',
                                                    'arguments': {'code': 's3cret-code'}, 'token': 'sweep-1'})
            job = acted['info']
            self.assertEqual((job['kind'], job['params']['action']), ('provisioners.act', 'sweep'))
            self.assertRegex(job['subject'], r'^provisioner://[0-9a-f]{32}$')
            _, output = serve.wait_job(job['id'])
            self.assertIn('sweeping top with [secret]', output)
            self.assertNotIn('s3cret-code', output)
            # The same token again is the same job, and nothing runs again.
            before = len(requests(self.folder))
            again = serve.call('provisioners.act', {'name': 'box', 'action': 'sweep',
                                                    'arguments': {'code': 's3cret-code'}, 'token': 'sweep-1'})
            self.assertEqual(again['job'], job['id'])
            self.assertEqual(len(requests(self.folder)), before)
            # And from a client of the server.
            client = Workstation('provisioners-client', ssh_key=False, trust=False)
            listed = client.rookery('--server', serve.url, '--token-stdin', 'provisioner', 'list', input=serve.token).stdout
            self.assertRegex(listed, r'(?m)^box\s+lab/box\s')
        finally:
            serve.stop()


@needs_e2e
class HostLifecycle(unittest.TestCase):
    """A host the lab provisioner made, stopped, started, duplicated and
    removed with its machine through the host's own actions; and a host no
    provisioner made, which has nothing to destroy."""

    @classmethod
    def setUpClass(cls):
        cls.station = Workstation('lifecycle', ssh_key=False, trust=False)
        cls.folder = lab(cls.station)
        cls.station.rookery('provisioner', 'run', 'box', 'settings', 'place=attic', '--yes')
        profile = cls.station.root / 'bench.json'
        profile.write_text(json.dumps({'builder': {}, 'access': {'user': 'ops'},
                                       'ansible_vars': {'rookery_install_tailscale': False}}))
        cls.station.rookery('host', 'import', 'bench', str(profile))
        # The machine is made, and setting it up stops as it has no address.
        made = cls.station.rookery('host', 'create', 'bench', '--provisioner', 'box', check=False)
        assert 'no address to reach it at' in made.stdout + made.stderr, made.stdout + made.stderr

    def rookery(self, *args, **kwargs):
        return self.station.rookery(*args, **kwargs)

    def host(self, name):
        hosts = json.loads(self.rookery('host', 'list', '-o', 'json').stdout)
        return next((host for host in hosts if host['name'] == name), None)

    def verbs(self):
        return [request['verb'] for request in requests(self.folder)]

    def test_1_it_is_stopped_and_started_through_its_provisioner(self):
        power = self.host('bench')['power']
        self.assertEqual((power['start_by'], power['stop_by']), ('provisioner', 'provisioner'))
        self.assertIn('stop', [line.split()[0] for line in self.rookery('action', 'list', 'bench').stdout.splitlines() if line.strip()])
        self.rookery('host', 'stop', 'bench', '--yes')
        self.assertEqual(self.verbs()[-1], 'stop')
        self.assertEqual(self.host('bench')['power']['state'], 'stopped')
        self.rookery('host', 'start', 'bench')
        self.assertEqual(self.verbs()[-1], 'start')
        self.assertEqual(self.host('bench')['power']['state'], 'running')

    def test_2_a_duplicate_has_its_machine_planned(self):
        self.rookery('host', 'duplicate', 'bench', 'shelf')
        shelf = self.host('shelf')
        self.assertEqual((shelf['machine']['provisioner'], shelf['machine']['state']), ('box', 'planned'))
        self.assertEqual(shelf['machine']['external_id'], '')
        profile = json.loads(self.rookery('host', 'export', 'shelf').stdout)
        self.assertNotIn('address', profile['access'])
        refused = self.rookery('host', 'stop', 'shelf', '--yes', check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('not made yet', refused.stderr)
        again = self.rookery('host', 'duplicate', 'bench', 'shelf', check=False)
        self.assertNotEqual(again.returncode, 0)
        self.assertIn('There is a host shelf already', again.stderr)
        self.rookery('host', 'remove', 'shelf', '--yes')
        self.assertIsNone(self.host('shelf'))

    def test_3_removing_it_with_destroy_destroys_its_machine(self):
        removed = self.rookery('host', 'remove', 'bench', '--destroy', '--yes')
        self.assertIn('box destroyed box-bench', removed.stdout + removed.stderr)
        self.assertEqual(self.verbs()[-1], 'destroy')
        self.assertIsNone(self.host('bench'))
        machines = json.loads(self.rookery('resource', 'list', '-o', 'json').stdout)
        self.assertFalse([machine for machine in machines if machine['kind'] == 'machine'], machines)

    def test_4_a_host_no_provisioner_made_has_nothing_to_destroy(self):
        profile = self.station.root / 'forge.json'
        profile.write_text(json.dumps({'type': 'ssh', 'access': {'address': '192.0.2.4', 'user': 'ops'}}))
        self.rookery('host', 'import', 'forge', str(profile))
        power = self.host('forge')['power']
        self.assertEqual((power['start'], power['stop']), (False, False))
        refused = self.rookery('host', 'remove', 'forge', '--destroy', '--yes', check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('No provisioner made forge', refused.stderr)
        self.assertIsNotNone(self.host('forge'))
        self.rookery('host', 'remove', 'forge', '--yes')
        self.assertIsNone(self.host('forge'))


@needs_e2e
class Deployments(unittest.TestCase):
    """A deployment of the lab provisioner, as a resource of its own."""

    @classmethod
    def setUpClass(cls):
        cls.station = Workstation('deployments', ssh_key=False, trust=False)
        cls.folder = lab(cls.station)
        cls.station.rookery('provisioner', 'run', 'box', 'settings', 'place=attic', '--yes')
        cls.station.rookery('secret', 'set', 'bench_token', input='b3nch-t0ken-made-up\n')

    def rookery(self, *args, **kwargs):
        return self.station.rookery(*args, **kwargs)

    def test_1_a_deployment_is_made_from_its_spec_with_a_secret_by_name(self):
        made = self.rookery('resource', 'create', 'bench', '--provisioner', 'box', '--spec', '-', '--secret', 'bench_token:TOKEN', input='benches: 3\n')
        output = made.stdout + made.stderr
        self.assertIn('deploying bench with [secret]', output)
        self.assertNotIn('b3nch-t0ken', output)
        self.assertEqual((self.folder / 'spec.bench').read_text(), 'benches: 3\n')
        # The secret reaches the program in the request on its input, and the spec is kept without it.
        create = next(asked for asked in requests(self.folder) if asked['verb'] == 'create')
        self.assertEqual(list(create['environment']), ['TOKEN'])
        self.assertNotIn('b3nch-t0ken', create['spec'] + json.dumps(create['arguments']))
        listed = self.rookery('resource', 'list').stdout
        self.assertRegex(listed, r'(?m)^[0-9a-f]{32}\s+deployment\s+bench\s+box\s+running\s*$')
        page = self.rookery('provisioner', 'show', 'box').stdout
        self.assertIn('Bench setup rookery made with it', page)
        self.assertRegex(page, r'  bench\s+project-bench  [0-9a-f]{32}\n')

    def test_2_its_page_actions_log_and_change(self):
        shown = self.rookery('resource', 'show', 'bench').stdout
        for line in ('State      running', 'Made as    project-bench', 'Secrets    bench_token as TOKEN',
                     '  services', '  change', '  remove'):
            self.assertIn(line, shown)
        said = self.rookery('resource', 'run', 'bench', 'services').stdout
        self.assertEqual(said.splitlines(), ['deployment project-bench has 1 service', '  web  running'])
        log = self.rookery('resource', 'logs', 'bench', '--tail', '5').stdout
        self.assertIn('project-bench started, tail 5', log)
        (self.station.root / 'bench.txt').write_text('benches: 4\n')
        self.rookery('resource', 'create', 'bench', '--provisioner', 'box', '--spec', str(self.station.root / 'bench.txt'))
        self.assertEqual((self.folder / 'spec.bench').read_text(), 'benches: 4\n')
        self.assertEqual(requests(self.folder)[-1]['verb'], 'change')

    def test_3_through_rookery_serve_with_a_token_asked_twice(self):
        serve = Serve(self.station)
        try:
            body = {'provisioner': 'box', 'kind': 'deployment', 'name': 'depot', 'spec': 'depot: 1\n',
                    'secrets': [{'name': 'bench_token', 'env': 'TOKEN'}], 'token': 'depot-1'}
            created = serve.call('resources.create', body)
            again = serve.call('resources.create', body)
            self.assertEqual((again['resource']['id'], again['job']['id']),
                             (created['resource']['id'], created['job']['id']))
            _, output = serve.wait_job(created['job']['id'])
            self.assertNotIn('b3nch-t0ken', output)
            rid = created['resource']['id']
            self.assertEqual(serve.call('resources.get', {'id': rid})['state'], 'running')
            listed = serve.call('resources.list', {'kind': 'deployment'})
            self.assertEqual(sorted(found['name'] for found in listed), ['bench', 'depot'])
            removed = serve.call('resources.remove', {'id': rid})
            serve.wait_job(removed['job'])
            self.assertEqual(serve.refused('resources.get', {'id': rid}), 'NOT_FOUND')
        finally:
            serve.stop()

    def test_4_removing_it_forgets_it(self):
        removed = self.rookery('resource', 'remove', 'bench', '-y')
        self.assertIn('Removed the deployment bench', removed.stdout)
        self.assertNotIn('bench', self.rookery('resource', 'list').stdout)

    def test_5_the_provisioners_own_action_makes_one_and_it_is_kept_while_it_holds_one(self):
        self.rookery('provisioner', 'run', 'box', 'new-deployment', 'name=shed', 'spec=shed: 1\nbenches: 2\n', 'secrets=bench_token:TOKEN', '--yes')
        self.assertEqual((self.folder / 'spec.shed').read_text(), 'shed: 1\nbenches: 2\n')
        self.assertIn('shed', self.rookery('resource', 'list').stdout)
        # Renamed, it keeps its settings and what it made.
        taken = self.rookery('provisioner', 'rename', 'box', 'old', check=False)
        self.assertNotEqual(taken.returncode, 0)
        self.assertIn('There is a provisioner old already', taken.stderr)
        self.assertIn('box is called bench-lab now', self.rookery('provisioner', 'rename', 'box', 'bench-lab').stdout)
        self.assertRegex(self.rookery('resource', 'list').stdout, r'(?m)^[0-9a-f]{32}\s+deployment\s+shed\s+bench-lab\s')
        self.assertIn('The lab at attic answers', self.rookery('provisioner', 'show', 'bench-lab').stdout)
        self.assertIn('State      running', self.rookery('resource', 'show', 'shed').stdout)
        self.assertNotEqual(self.rookery('provisioner', 'show', 'box', check=False).returncode, 0)
        refused = self.rookery('provisioner', 'remove', 'bench-lab', '-y', check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('bench-lab still holds', refused.stderr)
        self.rookery('resource', 'remove', 'shed', '-y')
        self.assertIn('Removed the provisioner bench-lab', self.rookery('provisioner', 'remove', 'bench-lab', '-y').stdout)
        self.assertNotIn('Lab box', self.rookery('provisioner', 'list').stdout)


def echo():
    """A listener on the loopback that sends back what it gets and ends its side after the
    caller has ended theirs: its port."""
    listener = socket.create_server(('127.0.0.1', 0))

    def serve():
        while True:
            connection, _ = listener.accept()
            with connection:
                while data := connection.recv(65536):
                    connection.sendall(data)
                connection.shutdown(socket.SHUT_WR)

    threading.Thread(target=serve, daemon=True).start()
    return listener.getsockname()[1]


def through_endpoint(serve, rid, endpoint, data):
    """Send `data` to an endpoint of a resource on a stream of /ws, end, and read what comes back
    until the other side has ended too; the error when it does not open."""
    socket = serve.socket()
    try:
        return socket.through('resources.connect', {'id': rid, 'endpoint': endpoint}, data)
    finally:
        socket.close()


def job_output(serve, job):
    """A job's output and the exit code it ended with."""
    ended, output = serve.wait_job(job)
    code = (ended.get('result') or {}).get('exit')
    if code is None:
        code = ((ended.get('error') or {}).get('details') or {}).get('exit')
    return output, code


@needs_e2e
class Endpoints(unittest.TestCase):
    """A deployment of the lab provisioner that says where it listens, reached through rookery
    serve, and a command run in it."""

    @classmethod
    def setUpClass(cls):
        cls.station = Workstation('endpoints', ssh_key=False, trust=False)
        cls.folder = lab(cls.station)
        cls.station.rookery('provisioner', 'set', 'box', 'place=attic')
        cls.serve = Serve(cls.station)
        cls.port = echo()
        body = {'provisioner': 'box', 'kind': 'deployment', 'name': 'db', 'spec': f'listen: {cls.port}\n'}
        created = cls.serve.call('resources.create', body)
        cls.rid = created['resource']['id']
        _, code = job_output(cls.serve, created['job']['id'])
        assert code == 0, code

    def test_its_endpoints_are_kept_with_it(self):
        seen = self.serve.call('resources.get', {'id': self.rid})
        self.assertEqual(seen['endpoints'], [{'name': 'bench', 'address': '127.0.0.1', 'port': self.port,
                                              'protocol': 'tcp', 'hint': 'echo', 'local': True}])

    def test_an_endpoint_is_reached_on_a_stream(self):
        data = os.urandom(1 << 20)
        self.assertEqual(through_endpoint(self.serve, self.rid, 'bench', b'hello'), b'hello')
        self.assertEqual(through_endpoint(self.serve, self.rid, str(self.port), data), data)
        # Only what the provisioner reported is opened.
        refused = through_endpoint(self.serve, self.rid, '22', b'')
        self.assertEqual(refused['error']['data']['code'], 'NOT_FOUND')

    def test_a_command_runs_in_it_as_a_job_that_ends_as_the_command_did(self):
        acted = self.serve.call('resources.act', {'id': self.rid, 'action': 'exec',
                                                  'arguments': {'command': 'count --all', 'stdin': 'benches'}})
        output, code = job_output(self.serve, acted['job'])
        self.assertEqual(code, 4, output)
        self.assertIn("ran count --all reading 'benches'", output)
        # The output is the command's alone.
        self.assertNotIn('exited with', output)

    def test_the_command_line_shows_its_endpoints(self):
        page = self.station.rookery('resource', 'show', 'db').stdout
        self.assertIn(f'Endpoints (rookery port forward {self.rid} ENDPOINT)', page)
        self.assertRegex(page, rf'(?m)^  bench  127\.0\.0\.1:{self.port}/tcp  echo · on the server\'s machine$')
        shown = json.loads(self.station.rookery('resource', 'show', 'db', '-o', 'json').stdout)
        self.assertEqual([endpoint['name'] for endpoint in shown['endpoints']], ['bench'])

    def test_the_command_line_runs_a_command_in_it_with_its_exit_status(self):
        ran = self.station.rookery('resource', 'exec', 'db', '--stdin', '--', 'count', '--all',
                                 input='benches', check=False)
        self.assertEqual(ran.returncode, 4, ran.output)
        self.assertIn("ran count --all reading 'benches'", ran.stdout)
        self.assertNotIn('exited with', ran.stdout)

    def test_port_forward_reaches_an_endpoint_by_its_name(self):
        free = socket.create_server(('127.0.0.1', 0))
        local = free.getsockname()[1]
        free.close()
        forward = self.station.spawn('port', 'forward', 'db', f'{local}:bench')
        try:
            said = forward.stdout.readline()
            self.assertIn(f'127.0.0.1:{local} → db:bench', said)
            with socket.create_connection(('127.0.0.1', local), timeout=10) as connection:
                connection.sendall(b'hello')
                connection.shutdown(socket.SHUT_WR)
                got = b''
                while data := connection.recv(65536):
                    got += data
            self.assertEqual(got, b'hello')
        finally:
            forward.terminate()
            forward.wait(timeout=10)
        # A name that is no host and no resource says so.
        refused = self.station.rookery('port', 'forward', 'nowhere', '80', check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('There is no host or resource nowhere', refused.stderr)


@needs_e2e
@unittest.skipUnless(os.environ.get('ROOKERY_E2E_DOCKER_HOST'), 'no Docker engine on the test network')
class DockerEngine(unittest.TestCase):
    """The Docker plugin against the engine of the test network."""

    def test_the_engine_answers_and_lists_its_runtimes(self):
        station = Workstation('provisioners-docker', ssh_key=False, trust=False)
        endpoint = os.environ['ROOKERY_E2E_DOCKER_HOST']
        station.rookery('provisioner', 'add', 'docker', 'docker/container', f'host={endpoint}')
        page = station.rookery('provisioner', 'show', 'docker').stdout
        self.assertIn(f'answers at {endpoint}', page)
        self.assertIn('Runtimes', page)
        volumes = station.rookery('provisioner', 'run', 'docker', 'volumes').stdout
        self.assertIn(f'volumes at {endpoint}', volumes)
        # The engine's images and volumes are tables of their own on the page.
        self.assertRegex(page, r'(?m)^Images \(\d+\)$')
        self.assertRegex(page, r'(?m)^Volumes \(\d+\)$')

    def test_a_compose_deployment_is_made_changed_and_removed(self):
        station = Workstation('provisioners-compose', ssh_key=False, trust=False)
        endpoint = os.environ['ROOKERY_E2E_DOCKER_HOST']
        station.rookery('provisioner', 'add', 'docker', 'docker/container', f'host={endpoint}')
        station.rookery('secret', 'set', 'compose_token', input='c0mpose-t0ken-made-up\n')
        spec = ('services:\n  idle:\n    image: busybox:1.37\n'
                '    command: ["sh", "-c", "echo token-is-$$TOKEN; sleep 600"]\n'
                '    environment:\n      TOKEN: ${TOKEN}\n')
        name = 'rookery-test-e2e'
        made = station.rookery('resource', 'create', name, '--provisioner', 'docker', '--spec', '-', '--secret', 'compose_token:TOKEN', input=spec, timeout=300)
        self.assertNotIn('c0mpose-t0ken', made.stdout + made.stderr)
        try:
            self.assertIn('running', station.rookery('resource', 'show', name).stdout)
            log = station.rookery('resource', 'logs', name, '--tail', '20').stdout
            self.assertIn('token-is-[secret]', log)
            station.rookery('resource', 'set', name, '--spec', '-', input=spec.replace('600', '601'), timeout=300)
            self.assertIn('running', station.rookery('resource', 'show', name).stdout)
        finally:
            removed = station.rookery('resource', 'remove', name, '-y', timeout=300)
        self.assertIn(f'Removed the deployment {name}', removed.stdout)

    def test_an_instance_is_reached_at_its_published_port_and_runs_a_command(self):
        station = Workstation('provisioners-instance', ssh_key=False, trust=False)
        endpoint = os.environ['ROOKERY_E2E_DOCKER_HOST']
        station.rookery('provisioner', 'add', 'docker', 'docker/container', f'host={endpoint}')
        serve = Serve(station)
        command = "sh -c 'mkdir -p /www && echo hello-from-box > /www/index.html && httpd -f -p 8080 -h /www'"
        body = {'provisioner': 'docker', 'kind': 'instance', 'name': 'rookery-test-web',
                'arguments': {'image': 'busybox:1.37', 'command': command, 'ports': '8080'}}
        created = serve.call('resources.create', body)
        rid = created['resource']['id']
        try:
            output, code = job_output(serve, created['job']['id'])
            self.assertEqual(code, 0, output)
            # The engine is elsewhere, so the port Docker chose is at the engine's own name.
            [published] = serve.call('resources.get', {'id': rid})['endpoints']
            host = endpoint.split('://', 1)[1].rsplit(':', 1)[0]
            self.assertEqual((published['address'], published['protocol']), (host, 'tcp'))
            self.assertNotIn('local', published)
            answer = through_endpoint(serve, rid, published['name'], b'GET / HTTP/1.0\r\n\r\n')
            self.assertIn(b'200 OK', answer)
            self.assertIn(b'hello-from-box', answer)
            answer = through_endpoint(serve, rid, str(published['port']), b'GET / HTTP/1.0\r\n\r\n')
            self.assertIn(b'hello-from-box', answer)
            acted = serve.call('resources.act', {
                'id': rid, 'action': 'exec',
                'arguments': {'command': '["sh", "-c", "cat /www/index.html; exit 5"]'}})
            output, code = job_output(serve, acted['job'])
            self.assertEqual(code, 5, output)
            self.assertIn('hello-from-box', output)
            # The same from the command line: its output and its exit status are the command's.
            ran = station.rookery('resource', 'exec', rid, '--', 'sh', '-c', 'cat /www/index.html; exit 5',
                                check=False)
            self.assertEqual((ran.returncode, ran.stdout.strip()), (5, 'hello-from-box'), ran.output)
        finally:
            removed = serve.call('resources.remove', {'id': rid})
            job_output(serve, removed['job'])


if __name__ == '__main__':
    unittest.main()
