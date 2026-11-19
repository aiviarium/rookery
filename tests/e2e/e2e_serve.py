"""rookery serve over TCP: its methods over /rpc and /ws with their streams, grants
on a host for people who are not admins, terminals, files, ports and the desktop
of a host, setting the server up through the API, and applying as a job.

All but ApplyJob use rookery serve as harness.served() starts it. ApplyJob
looks at the job of the server's own shared state (harness.served_extended())."""
import base64
import http.client
import json
import struct
import unittest
import urllib.parse

from harness import (AGENT, HOST, WebSocket, needs_e2e, provisioned, random_text, served, served_extended,
                     shell_quote, unb64, wait_for)


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
                if allowed:
                    # The host may still say no, as units do without a working systemd: that is
                    # the host's answer, never the server's own failure.
                    self.assertIn(refused, (None, 'UNAVAILABLE'), (level, method))
                else:
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
class Connect(unittest.TestCase):
    """A host's terminals, files, ports and desktop through the server, for the levels on it."""

    @classmethod
    def setUpClass(cls):
        cls.serve = served()
        cls.people = {}
        for level in ('view', 'use'):
            token = cls.serve.make_token(f'cleo-{level}')
            user = cls.serve.call('auth.me', token=token)['user']['id']
            cls.serve.call('access.grant', {'resource': f'env://{HOST}', 'principal': f'user:{user}',
                                            'level': level})
            cls.people[level] = token

    def test_a_terminal_is_its_owners(self):
        use, view = self.people['use'], self.people['view']
        self.assertEqual(self.serve.refused('terminals.open', {'env': f'env://{HOST}'}, token=view), 'FORBIDDEN')
        opened = self.serve.call('terminals.open', {'env': f'env://{HOST}', 'command': ['cat']}, token=use)
        self.assertEqual(opened['level'], 'use')
        self.assertEqual(self.serve.refused('terminals.get', {'id': opened['id']}, token=view), 'FORBIDDEN')
        self.assertEqual(self.serve.refused('terminals.get', {'id': opened['id']}), None)
        marker = random_text('typed')
        socket = self.serve.socket(token=use)
        try:
            attached = socket.result('terminals.attach', {'id': opened['id'], 'size': {'cols': 100, 'rows': 30}})
            self.assertEqual(attached['mode'], 'snapshot')
            socket.send(attached['stream'], b'\x00' + marker.encode() + b'\n')
            tag = struct.pack('!I', attached['stream'])
            seen = b''

            def output(frame):
                return isinstance(frame, bytes) and frame[:4] == tag

            while marker.encode() not in seen:
                frame = socket._take(output, 30)
                if frame[4] == 0:
                    seen += frame[5:]
        finally:
            socket.ws.close()
        presence = self.serve.call('env.presence', {'env': HOST}, token=view)['users']
        self.assertEqual([user['name'] for user in presence], ['cleo-use'])
        self.serve.call('terminals.close', {'id': opened['id']}, token=use)

    def test_files_and_ports_need_use(self):
        use, view = self.people['use'], self.people['view']
        path = f'/home/{AGENT}/{random_text("connect")}.txt'
        written = self.serve.call('env.writeFile', {'env': HOST, 'path': path,
                                                    'base64': base64.b64encode(b'one').decode()}, token=use)
        read = self.serve.call('env.readFile', {'env': HOST, 'path': path}, token=use)
        self.assertEqual((base64.b64decode(read['base64']), read['sha256']), (b'one', written['sha256']))
        self.assertEqual(self.serve.refused('env.readFile', {'env': HOST, 'path': path}, token=view), 'FORBIDDEN')
        self.serve.call('env.deleteFile', {'env': HOST, 'path': path}, token=use)
        # A web server of the agent's on the host's loopback, opened in a browser on its own origin.
        port = 18765
        self.serve.call('hosts.sessions.open', {'host': HOST, 'kind': 'process', 'user': 'agent', 'command': [
            'python3', '-m', 'http.server', str(port), '--bind', '127.0.0.1', '--directory', '/tmp']})

        def get(path, headers):
            connection = http.client.HTTPConnection('127.0.0.1', self.serve.port, timeout=60)
            try:
                connection.request('GET', path, headers=headers)
                answer = connection.getresponse()
                return answer.status, answer.headers, answer.read()
            finally:
                connection.close()

        status, _, _ = get(f'/env/{HOST}/port/{port}/', {'Authorization': f'Bearer {view}'})
        self.assertEqual(status, 403)

        def redirected():
            status, headers, _ = get(f'/env/{HOST}/port/{port}/', {'Authorization': f'Bearer {use}'})
            return headers['Location'] if status in (302, 303, 307) else None
        location = urllib.parse.urlsplit(wait_for('the port\'s address', redirected))
        claimed, headers, _ = get(f'{location.path}?{location.query}', {'Host': location.netloc})
        self.assertIn(claimed, (302, 303, 307))
        cookie = headers['Set-Cookie'].split(';')[0]

        def page():
            status, _, body = get('/', {'Host': location.netloc, 'Cookie': cookie})
            return body if status == 200 else None
        self.assertIn(b'Directory listing', wait_for('the port\'s page', page))
        status, _, _ = get('/', {'Host': location.netloc})
        self.assertEqual(status, 401)

    def test_commands_and_checkouts_need_use(self):
        use, view = self.people['use'], self.people['view']
        home = f'/home/{AGENT}'
        ran = self.serve.call('env.exec', {'env': HOST, 'command': ['pwd'], 'cwd': home}, token=use)
        self.assertEqual((ran['exitCode'], ran['stdout'].strip()), (0, home))
        self.assertEqual(self.serve.refused('env.exec', {'env': HOST, 'command': ['id']},
                                            token=view), 'FORBIDDEN')
        self.assertEqual(self.serve.refused('env.exec', {'env': HOST, 'command': ['id'], 'cwd': '/etc'},
                                            token=use), 'FORBIDDEN')
        self.assertEqual(self.serve.refused('env.exec', {'env': HOST, 'command': ['sh', '-c', 'sleep 30'],
                                                          'timeoutMs': 200}, token=use), 'TIMEOUT')

        # A repository on the host, cloned into the folder the host keeps for checkouts.
        name = random_text('ws')
        source = f'{home}/{name}-src'
        made = self.serve.call('env.exec', {'env': HOST, 'command': ['sh', '-c', (
            f'rm -rf {source} && mkdir -p {source} && cd {source} && git init -q -b main && '
            'git -c user.name=Test -c user.email=test@example.test commit -q --allow-empty -m one'
        )]}, token=use)
        self.assertEqual(made['exitCode'], 0, made['stderr'])
        self.assertEqual(self.serve.refused('env.clone', {'env': HOST, 'repository': source},
                                            token=view), 'FORBIDDEN')
        self.assertEqual(self.serve.refused('env.clone', {'env': HOST, 'repository': source,
                                                           'name': '../out'}, token=use), 'INVALID')
        self.assertEqual(self.serve.refused('env.clone', {'env': HOST, 'repository': 'ext::sh -c echo'},
                                            token=use), 'INVALID')
        clone = self.serve.call('env.clone', {'env': HOST, 'repository': source,
                                              'name': f'{name}-clone'}, token=use)
        self.assertEqual((clone['name'], clone['branch'], clone['env']),
                         (f'{name}-clone', 'main', f'env://{HOST}'))
        self.assertTrue(clone['path'].startswith('/srv/agent/workspaces/'), clone['path'])
        worktree = self.serve.call('env.worktree', {'env': HOST, 'checkout': clone['path'],
                                                    'branch': f'{name}/one'}, token=use)
        self.assertEqual((worktree['branch'], worktree['worktreeOf']),
                         (f'{name}/one', clone['path']))

        # Listing and removing checkouts need use.
        listed = [c['name'] for c in self.serve.call('env.checkouts', {'env': HOST},
                                                     token=use)['checkouts']]
        self.assertIn(f'{name}-clone', listed)
        self.assertIn(worktree['name'], listed)
        self.assertEqual(self.serve.refused('env.checkouts', {'env': HOST}, token=view), 'FORBIDDEN')
        self.assertEqual(self.serve.refused('env.checkouts', {'env': HOST},
                                            token=self.serve.token), None)
        self.assertEqual(self.serve.refused('env.removeCheckout', {'env': HOST, 'path': clone['path']},
                                            token=view), 'FORBIDDEN')
        self.serve.call('env.removeCheckout', {'env': HOST, 'path': worktree['path'], 'force': True},
                        token=use)
        self.serve.call('env.removeCheckout', {'env': HOST, 'path': clone['path'], 'force': True},
                        token=use)
        listed = [c['name'] for c in self.serve.call('env.checkouts', {'env': HOST},
                                                     token=use)['checkouts']]
        self.assertNotIn(f'{name}-clone', listed)
        self.serve.call('env.exec', {'env': HOST, 'command': ['rm', '-rf', source]}, token=use)

    def test_the_desktop_is_for_use(self):
        use, view = self.people['use'], self.people['view']
        station = provisioned()
        # In the headless CI container, desktop packages (scrot, TigerVNC) are not installed.
        # Provide minimal temporary helpers only if absent, leaving real desktop VMs untouched.
        has_scrot = station.root_on_host(
            f'su -s /bin/sh {AGENT} -c "/usr/local/bin/agent-env sh -c \'command -v scrot >/dev/null\'"',
            check=False,
        ).returncode == 0
        has_vnc = station.root_on_host(
            "python3 -c \"import socket; socket.create_connection(('127.0.0.1', 5901), 1).close()\"",
            check=False,
        ).returncode == 0
        scrot_path = f'/home/{AGENT}/.local/bin/scrot'
        vnc_session = None
        if not has_scrot:
            png_b64 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8BQDwAEgAF/posBPQAAAABJRU5ErkJggg=='
            script = f'#!/bin/sh\nprintf %s {png_b64} | base64 -d > "$1"\n'
            station.root_on_host(
                f'install -d -o {AGENT} -g {AGENT} /home/{AGENT}/.local/bin && '
                f'printf %s {shell_quote(script)} > {scrot_path} && '
                f'chown {AGENT}:{AGENT} {scrot_path} && chmod 0755 {scrot_path}'
            )
        if not has_vnc:
            rfb = (
                'import socket, struct\n'
                'def exact(c, n):\n'
                ' b = b""\n'
                ' while len(b) < n:\n'
                '  chunk = c.recv(n - len(b))\n'
                '  if not chunk: raise EOFError()\n'
                '  b += chunk\n'
                ' return b\n'
                's = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n'
                's.bind(("127.0.0.1", 5901)); s.listen(4)\n'
                'while True:\n'
                ' c, _ = s.accept()\n'
                ' try:\n'
                '  c.sendall(b"RFB 003.008\\n"); exact(c, 12)\n'
                '  c.sendall(b"\\x01\\x01"); exact(c, 1)\n'
                '  c.sendall(b"\\x00\\x00\\x00\\x00"); exact(c, 1)\n'
                '  c.sendall(struct.pack("!HH", 800, 600) + b"\\x00" * 20); c.recv(1)\n'
                ' except (OSError, EOFError): pass\n'
                ' finally: c.close()\n'
            )
            vnc_session = self.serve.call('hosts.sessions.open', {
                'host': HOST, 'kind': 'process', 'user': 'agent', 'command': ['python3', '-c', rfb],
            })['id']
            wait_for('temporary VNC server on 5901', lambda: station.root_on_host(
                "python3 -c \"import socket; socket.create_connection(('127.0.0.1', 5901), 1).close()\"",
                check=False,
            ).returncode == 0)
        try:
            # View sees a picture of it, and only use the desktop itself.
            picture = self.serve.call('hosts.desktop.screenshot', {'host': HOST}, token=view)
            self.assertTrue(base64.b64decode(picture['base64']).startswith(b'\x89PNG'), picture.get('content_type'))
            with self.assertRaises(PermissionError):
                WebSocket(self.serve.port, f'/env/{HOST}/desktop', protocols=['binary'],
                          headers={'Authorization': f'Bearer {view}'})
            desktop = WebSocket(self.serve.port, f'/env/{HOST}/desktop', protocols=['binary'],
                                headers={'Authorization': f'Bearer {use}'})
            try:
                # The server answered the VNC password itself; the viewer is offered no security.
                self.assertEqual(desktop.recv(), b'RFB 003.008\n')
                desktop.send(b'RFB 003.008\n')
                self.assertEqual(desktop.recv(), bytes([1, 1]))
                desktop.send(bytes([1]))
                self.assertEqual(desktop.recv(), bytes(4))
                desktop.send(bytes([1]))
                init = desktop.recv()
                width, height = struct.unpack('!HH', init[:4])
                self.assertGreater(width * height, 0)
            finally:
                desktop.close()
        finally:
            if not has_scrot:
                station.root_on_host(f'rm -f {scrot_path}', check=False)
            if vnc_session:
                self.serve.call('hosts.sessions.close', {'host': HOST, 'terminal': vnc_session})


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
