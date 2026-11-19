"""rookery serve over TCP: its methods over /rpc and /ws with their streams, grants
on a host for people who are not admins, setting the server up through the API,
and applying as a job.

All but ApplyJob use rookery serve as harness.served() starts it. ApplyJob
looks at the job of the server's own shared state (harness.served_extended())."""
import base64
import json
import unittest

from harness import (AGENT, HOST, needs_e2e, provisioned, random_text, served, served_extended, unb64,
                     wait_for)


def text_of(started):
    return unb64(started.get('stdout_b64'))


def process(command, user='agent'):
    return {'host': HOST, 'kind': 'process', 'command': command, 'user': user, 'wait': '30s'}


@needs_e2e
class Methods(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.serve = served()

    def test_the_server_and_its_hosts(self):
        me = self.serve.call('auth.me')
        self.assertTrue(me['admin'])
        self.assertTrue(me['version'])
        hosts = wait_for('the host answering through the server', lambda: [
            host for host in self.serve.call('hosts.list')['hosts'] if host['name'] == HOST and host['reachable']])
        self.assertTrue(hosts[0]['version'])
        self.assertEqual(self.serve.call('hosts.get', {'host': f'env://{HOST}'})['address'], HOST)

    def test_nothing_without_a_token(self):
        for token in ('', 'rookery_pat_not-a-token', self.serve.token[:-1]):
            status, answer = self.serve.rpc('hosts.list', token=token)
            self.assertEqual(status, 401, answer)
            self.assertEqual(answer['error']['data']['code'], 'UNAUTHENTICATED')

    def test_a_hosts_operations_go_through(self):
        started = self.serve.call('hosts.sessions.open', process(['id', '-un']))
        self.assertEqual((started['exit_code'], text_of(started)), (0, f'{AGENT}\n'))
        actions = self.serve.call('hosts.actions', {'host': HOST})['actions']
        self.assertIn('secrets', [action['name'] for action in actions])
        path = f'/home/{AGENT}/through-serve.txt'
        written = b'written through rookery serve'
        self.serve.call('hosts.files.write', {'host': HOST, 'path': path, 'base64': base64.b64encode(written).decode()})
        read = self.serve.call('hosts.files.read', {'host': HOST, 'path': path})
        self.assertEqual(base64.b64decode(read['base64']), written)
        self.assertEqual(self.serve.refused('hosts.sessions.list', {'host': 'nowhere'}), 'NOT_FOUND')

    def test_the_key_value_storage(self):
        key = random_text('key')
        first = self.serve.call('storage.set', {'plugin': 'e2e', 'key': key, 'value': {'colour': 'blue'}})
        self.assertEqual(self.serve.call('storage.get', {'plugin': 'e2e', 'key': key})['value'],
                         {'colour': 'blue'})
        self.assertEqual(self.serve.refused('storage.set', {'plugin': 'e2e', 'key': key, 'value': {'colour': 'red'},
                                                            'revision': first['revision'] + 100}), 'CONFLICT')
        self.serve.call('storage.set', {'plugin': 'e2e', 'key': key, 'value': {'colour': 'red'}})
        listed = self.serve.call('storage.list', {'plugin': 'e2e'})['entries']
        self.assertIn(key, [entry['key'] for entry in listed])
        self.serve.call('storage.delete', {'plugin': 'e2e', 'key': key})
        self.assertIsNone(self.serve.call('storage.get', {'plugin': 'e2e', 'key': key})['value'])

    def test_what_the_host_keeps_is_taken_in(self):
        body = {'note': random_text('inbox')}
        self.serve.call('hosts.sessions.open', process(['box', 'publish', 'e2e/inbox', json.dumps(body), '--keep', 'log']))

        def arrived():
            listed = self.serve.call('hosts.messages.list', {'host': HOST, 'topic': 'e2e/inbox'})['messages']
            return [item for item in listed if item['body'] == body]
        found = wait_for('the message taken in', arrived)[0]
        number = found['id']
        self.serve.call('hosts.messages.done', {'host': HOST, 'id': number})
        self.assertTrue(arrived()[0]['done'])
        self.serve.call('hosts.messages.reopen', {'host': HOST, 'id': number})
        self.assertFalse(arrived()[0]['done'])
        # The command line marks it the same way.
        rookery = self.serve.station.rookery
        self.assertIn(f'Message {number} of {HOST} is done', rookery('inbox', 'done', HOST, str(number)).output)
        self.assertTrue(arrived()[0]['done'])
        self.assertNotIn(body['note'], rookery('inbox', 'list', HOST, '--topic', 'e2e/inbox').stdout)
        rookery('inbox', 'reopen', HOST, str(number))
        self.assertFalse(arrived()[0]['done'])
        # It is on the host's durable topic too, by the host's id.
        host = self.serve.call('hosts.get', {'host': HOST})
        topic = self.serve.call('topics.inspect', {'plugin': 'rookery', 'topic': f'hosts.{host["id"]}.channel'})
        self.assertGreater(topic['messages'], 0, topic)


@needs_e2e
class Streams(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.serve = served()

    def setUp(self):
        self.socket = self.serve.socket()

    def tearDown(self):
        self.socket.close()

    def test_calls_over_the_socket(self):
        self.assertIn(HOST, [host['name'] for host in self.socket.result('hosts.list')['hosts']])
        started = self.socket.result('hosts.sessions.open', process(['echo', 'through /ws']))
        self.assertEqual(text_of(started), 'through /ws\n')
        missing = self.socket.call('nothing.here')
        self.assertEqual(missing['error']['code'], -32601)

    def test_a_session_streams_its_output(self):
        marker = random_text('streamed')
        started = self.socket.result('hosts.sessions.open', {
            'host': HOST, 'kind': 'process', 'command': ['sh', '-c', f'sleep 1; echo {marker}']})
        data, _ = self.socket.stream('hosts.sessions.attach', {'host': HOST, 'terminal': started['id']},
                                     until=lambda data: marker.encode() in data)
        self.assertIn(marker.encode(), data)


@needs_e2e
class Grants(unittest.TestCase):
    """People who are not admins do on a host what their grant on env://NAME allows."""

    @classmethod
    def setUpClass(cls):
        cls.serve = served()
        cls.people = {}
        for level in ('none', 'view', 'use', 'manage'):
            name = f'grace-{level}'
            token = cls.serve.make_token(name)
            user = cls.serve.call('auth.me', token=token)['user']['id']
            if level != 'none':
                cls.serve.call('access.grant', {'resource': f'env://{HOST}', 'principal': f'user:{user}',
                                                'level': level})
            cls.people[level] = token

    def test_each_level_may_do_what_it_names(self):
        asked = [
            ('hosts.get', {'host': HOST}, 'view'),
            ('hosts.sessions.list', {'host': HOST}, 'view'),
            ('hosts.sessions.open', process(['true']), 'use'),
            ('hosts.sessions.open', process(['true'], user='operator'), 'manage'),
            ('hosts.profile', {'host': HOST}, 'manage'),
            ('hosts.units.list', {'host': HOST}, 'view'),
            ('hosts.processes.list', {'host': HOST}, 'use'),
            ('hosts.logs.read', {'host': HOST, 'query': {'lines': '5'}}, 'manage'),
            ('installation.secrets.list', {}, 'admin'),
            ('keys.list', {}, 'admin'),
        ]
        order = ['none', 'view', 'use', 'manage', 'admin']
        for level, token in self.people.items():
            for method, params, needed in asked:
                refused = self.serve.refused(method, params, token=token)
                allowed = order.index(level) >= order.index(needed)
                self.assertEqual(refused is None, allowed, (level, method, refused))
                if not allowed:
                    self.assertEqual(refused, 'FORBIDDEN', (level, method))

    def test_a_person_sees_the_hosts_they_were_granted(self):
        none = self.serve.call('hosts.list', token=self.people['none'])['hosts']
        self.assertEqual(none, [])
        viewed = self.serve.call('hosts.list', token=self.people['view'])['hosts']
        self.assertEqual([host['name'] for host in viewed], [HOST])

    def test_an_action_needs_the_role_it_names(self):
        # Removing a host is an admin's; managing it is not enough. The refusal runs nothing.
        self.assertEqual(self.serve.refused('hosts.act', {'host': HOST, 'action': 'remove'},
                                            token=self.people['manage']), 'FORBIDDEN')


@needs_e2e
class SetUp(unittest.TestCase):
    """What an admin sets the server up with, over the API."""

    @classmethod
    def setUpClass(cls):
        cls.serve = served()

    def test_secrets_and_registries(self):
        name = random_text('token')
        self.serve.call('installation.secrets.set', {'name': name, 'value': 'kept on the server'})
        self.assertIn(name, self.serve.call('installation.secrets.list')['names'])
        self.serve.call('installation.secrets.remove', {'name': name})
        self.assertNotIn(name, self.serve.call('installation.secrets.list')['names'])
        registry = {'type': 'npm', 'url': 'http://192.0.2.60/npm/'}
        self.serve.call('registries.save', {'name': 'e2e-npm', 'value': registry})
        kept = self.serve.call('registries.list')['registries']['e2e-npm']
        self.assertRegex(kept.pop('id'), r'^[0-9a-f]{32}$')
        self.assertEqual(kept, registry)
        # A registry is a connection of the type registry.
        connections = self.serve.call('connections.list', {'type': 'registry'})['connections']
        self.assertIn('e2e-npm', [connection['name'] for connection in connections])
        self.serve.call('registries.remove', {'name': 'e2e-npm'})
        self.assertNotIn('e2e-npm', self.serve.call('registries.list')['registries'])

    def test_what_a_new_host_starts_from(self):
        catalogue = self.serve.call('setup.catalogue')
        self.assertIn('operator_ssh_public_keys', catalogue['template'])
        self.assertIn('github/cli', [feature['key'] for feature in catalogue['features']])
        own = self.serve.station.own_public()
        self.assertNotIn('key', catalogue)
        keys = {key['name']: key for key in self.serve.call('keys.list')['keys']}
        self.assertEqual(keys['rookery']['public'], own)
        self.assertNotIn('PRIVATE KEY', json.dumps(keys))
        name = self.serve.call('setup.generate', {'what': 'hostname'})['value']
        self.assertRegex(name, r'^rookery-')
        made = self.serve.call('setup.generate', {'what': 'password', 'host': 'draft', 'user': 'ops'})
        self.assertTrue(made['value'].startswith('$6$') and made['password'], made)
        hashed = self.serve.call('setup.generate', {'what': 'password', 'password': 'correct horse'})
        self.assertTrue(hashed['value'].startswith('$6$'))

    def test_a_host_is_made_seeded_and_removed(self):
        profile = provisioned().read_profile()
        # A seed of a host that joins the tailnet needs a key no test network can make.
        apart = {**profile.get('ansible_vars', {}), 'rookery_install_tailscale': False}
        self.serve.call('hosts.save', {'host': 'cloud-2', 'profile': {
            **profile, 'hostname': 'cloud-2', 'builder': {}, 'access': {}, 'ansible_vars': apart,
            'operator_password_hash': '$6$e2e$' + 'x' * 86,
            'release_url': 'https://192.0.2.30/rookery-release.tar.gz'}})
        self.assertFalse(self.serve.call('hosts.get', {'host': 'cloud-2'})['reachable'])
        seed = self.serve.call('hosts.act', {'host': 'cloud-2', 'action': 'seed'})['result']['user_data']
        own = self.serve.station.own_public().split()[1]
        self.assertTrue(seed.startswith('#cloud-config'))
        self.assertIn(own, seed)
        self.serve.call('hosts.remove', {'host': 'cloud-2'})
        self.assertEqual(self.serve.refused('hosts.get', {'host': 'cloud-2'}), 'NOT_FOUND')

    def test_the_server_sends_its_secrets_to_the_host(self):
        value = random_text('from-the-server')
        self.serve.call('installation.secrets.set', {'name': 'server-token', 'value': value})
        profile = self.serve.call('hosts.profile', {'host': HOST})['profile']
        kept = profile.get('secrets', [])
        profile['secrets'] = [{'name': 'server-token', 'env': 'SERVER_TOKEN'}]
        self.serve.call('hosts.save', {'host': HOST, 'profile': profile})
        try:
            acted = self.serve.call('hosts.act', {'host': HOST, 'action': 'secrets'})
            if acted.get('job'):
                job, log = self.serve.wait_job(acted['job'])
                self.assertEqual(job['state'], 'succeeded', log)
            else:
                session = acted['session']
                wait_for('the secrets action to end', lambda: self.serve.call(
                    'hosts.sessions.get', {'host': HOST, 'terminal': session['id']}).get('exit_code') == 0)
            env = provisioned().root_on_host(f'cat /home/{AGENT}/.config/rookery/secrets.env').stdout
            self.assertIn(f'export SERVER_TOKEN={value}', env)
        finally:
            profile['secrets'] = kept
            self.serve.call('hosts.save', {'host': HOST, 'profile': profile})

    def test_settings(self):
        self.serve.call('installation.settings.set', {'name': 'retention_days', 'value': '30'})
        settings = {setting['key']: setting for setting in self.serve.call('installation.settings.list')}
        self.assertEqual(settings['retention_days']['value'], '30')
        # One that belongs to the server's own machine is set there, over its socket.
        self.assertEqual(self.serve.refused('installation.settings.set', {'name': 'source', 'value': '/tmp'}),
                         'FORBIDDEN')


@needs_e2e
class ApplyJob(unittest.TestCase):
    """Applying as a job of the server, followed over /ws: the second apply of
    the shared state of rookery serve (harness.served_extended())."""

    @classmethod
    def setUpClass(cls):
        cls.applied = served_extended()
        cls.serve = cls.applied.serve

    def test_applying_is_a_job_of_the_server(self):
        job, texts, data = self.applied.job, self.applied.texts, self.applied.data
        ended = texts[-1]
        self.assertEqual(ended['state'], 'succeeded', data[-3000:])
        self.assertIn('PLAY RECAP', data)
        jobs = self.serve.call('jobs.list', {'subject': f'env://{HOST}'})['jobs']
        self.assertIn(job, [known['id'] for known in jobs])
        self.assertEqual([known['kind'] for known in jobs if known['id'] == job], ['hosts.act'])
        self.assertIn('failed=0', self.serve.log(job))
        # The operator now takes the server's key as well.
        keys = provisioned().ssh('ops', 'cat ~/.ssh/authorized_keys').stdout
        own = self.serve.station.own_public()
        self.assertIn(own, keys)
