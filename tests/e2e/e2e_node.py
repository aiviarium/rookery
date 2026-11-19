"""A node on the bare test host `forge`: rookery applies it without a desktop,
with a made-up plugin's feature for a node that offers a provisioner, and
registers that provisioner, naming the node. Applying again changes nothing
of it; a feature that is taken out takes its provisioner along, but not while
it holds a deployment; and the node is not forgotten while one is there. The
endpoints of a deployment of that provisioner are reached through the node's
controller, on its loopback and on its own network.

The test host is a container without systemd and without the internet, so
tests/e2e/site.yml applies the node's tasks that need neither: the guard,
its SSH, the features, rookeryd and the record of the apply. The packages, the
firewall and Tailscale are left out, and so are the features incus/server and
docker/engine, whose engines do not run here: tests/test_node.py reads them,
and runs the firewall's helper and the server's upgrade against fakes.

It runs last on `forge`, as it leaves a node behind."""
import json
import unittest

from harness import (HOST, OPERATOR, SCRATCH, Serve, Workstation, hand_over_operator_key, needs_e2e,
                     plugin_package, shell_quote, wait_for)

# The port of the listener on the node that sends back what it gets.
ECHO = 7071

# A provisioner that makes deployments and says nothing else of note.
# A deployment's endpoints are a listener on the node's loopback, the same
# one at the node's own address, and an address no network of the node holds.
PROGRAM = r'''#!/usr/bin/env python3
import json, socket, sys
verb = sys.argv[1]
request = json.load(sys.stdin)
with open(__file__ + '.node', 'w') as said:
    said.write(request.get('node', ''))
if verb == 'status' and not request.get('id'):
    print(json.dumps({'message': 'The lathe at ' + request['arguments'].get('place', 'nowhere') + ' answers'}))
elif verb in ('create', 'change'):
    print(json.dumps({'id': 'load-' + request['name'], 'state': 'running'}))
elif verb == 'status':
    endpoints = [{'name': 'db', 'address': '127.0.0.1', 'port': ECHO, 'local': True},
                 {'name': 'lan', 'address': socket.gethostbyname(HOST), 'port': ECHO, 'local': True},
                 {'name': 'far', 'address': '192.0.2.1', 'port': ECHO, 'local': True}]
    print(json.dumps({'state': 'running', 'endpoints': endpoints}))
elif verb == 'destroy':
    print(json.dumps({'state': 'gone'}))
else:
    print(json.dumps({}))
'''

# The lathe's tasks: what a feature of a node does to offer its provisioner.
TASKS = '''---
- name: Keep the certificate rookery gave the lathe to trust
  ansible.builtin.copy:
    dest: /etc/depot-lathe.crt
    content: "{{ rookery_client_certificates[rookery_plugin_feature].certificate }}"
    mode: '0644'
- name: Offer the lathe's provisioner
  ansible.builtin.set_fact:
    rookery_node_units: "{{ rookery_node_units + ['depot-lathe'] }}"
    rookery_offer:
      settings:
        place: "https://{address}:9999"
      secrets:
        depot_server_cert: "PUBLIC-CERT-OF-THE-LATHE"
      fingerprint: "0123abcd"
'''

MANIFEST = {
    'features': [
        {'id': 'lathe', 'label': 'Lathe', 'types': ['node'], 'offers': ['box'],
         'trusts': {'certificate': 'depot_client_cert', 'key': 'depot_client_key'},
         'tasks': 'tasks/lathe.yml',
         'actions': [{'name': 'lathe-status', 'label': 'Lathe', 'user': 'root', 'extends': 'status',
                      'run': 'echo lathe'}]},
        {'id': 'bench', 'label': 'Bench'},
    ],
    'provisioners': [{
        'id': 'box', 'label': 'Depot box', 'command': 'bin/provision', 'protocol': 2,
        'secrets': ['depot_client_cert', 'depot_client_key', {'name': 'depot_server_cert', 'required': False}],
        'arguments': [{'name': 'place', 'label': 'Place', 'required': True}],
        'kinds': [{'id': 'deployment', 'label': 'Load', 'spec': {'format': 'text', 'label': 'Load'}}],
    }],
}


@needs_e2e
class Node(unittest.TestCase):
    """The tests build on each other, in the order of their names."""

    # What the workstations of the tests before this one left on `forge`: a node
    # is not applied over a workstation that rookery set up, so it goes first.
    LEFT = ('pkill -x rookeryd; userdel -r agent; rm -rf /etc/rookery /var/lib/rookeryd /var/lib/rookery '
            '/etc/sudoers.d/90-rookery-agent /etc/ssh/sshd_config.d/00-rookery.conf; true')

    @classmethod
    def setUpClass(cls):
        cls.station = Workstation('node', ssh_key=False)
        package = SCRATCH / 'depot'
        program = PROGRAM.replace('ECHO', str(ECHO)).replace('HOST', repr(HOST))
        target = plugin_package(package, 'depot', MANIFEST,
                                {'bin/provision': program, 'tasks/lathe.yml': TASKS})
        (target / 'bin/provision').chmod(0o755)
        cls.station.rookery('plugin', 'add', str(package))
        hand_over_operator_key(cls.station.own_public())
        key = cls.station.own_key_file()
        wait_for('the operator taking the key of rookery', lambda: cls.station.ssh(
            OPERATOR, 'true', key=key, check=False).returncode == 0, timeout=60)
        cls.station.ssh(OPERATOR, f'sudo -n sh -c {shell_quote(cls.LEFT)}', key=key, check=False)
        cls.station.rookery('host', 'add', HOST, HOST, '--user', OPERATOR, '--type', 'node',
                          '--feature', 'depot/lathe', input='')

    @classmethod
    def tearDownClass(cls):
        hand_over_operator_key()

    def rookery(self, *args, **kwargs):
        kwargs.setdefault('input', '')
        return self.station.rookery(*args, **kwargs)

    def root(self, command, check=True):
        return self.station.ssh(OPERATOR, f'sudo -n sh -c {shell_quote(command)}', key=self.station.own_key,
                                check=check)

    def profile(self, features):
        profile = self.station.read_profile()
        profile['ansible_vars'] = {'rookery_plugin_features': list(features), 'rookery_start_services': False,
                                   'rookery_install_tailscale': False, 'rookery_manage_firewall': False}
        self.station.write_profile(profile)
        return profile

    def provisioners(self):
        listed = json.loads(self.rookery('provisioner', 'list', '-o', 'json').stdout)
        if isinstance(listed, dict):
            listed = listed['provisioners']
        return [provisioner['name'] for provisioner in listed]

    def test_1_a_feature_for_a_workstation_is_refused_before_anything_runs(self):
        added = self.station.read_profile()
        self.assertEqual(added['type'], 'node')
        self.assertEqual(added['ansible_vars']['rookery_plugin_features'], ['depot/lathe'])
        # A feature that is not for a node is refused when it is set, too.
        refused = self.rookery('host', 'set', HOST, 'feature.depot/bench=on', check=False)
        self.assertEqual(refused.returncode, 2, refused.output)
        self.assertIn('not for a host of the type node', refused.stderr)
        self.profile(['depot/lathe', 'depot/bench'])
        refused = self.rookery('apply', HOST, check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn(f'{HOST} is a node, and depot/bench is for a workstation', refused.stderr)
        self.assertNotEqual(self.root('test -e /etc/rookery/applied.json', check=False).returncode, 0)

    def test_2_applying_the_node_registers_what_its_feature_offers(self):
        self.profile(['depot/lathe'])
        done = self.rookery('apply', HOST, timeout=900)
        name = f'{HOST}-depot-box'
        self.assertIn(f'✓ Registered the provisioner {name} (depot/box) of {HOST}', done.stdout)
        self.assertIn('pins the server\'s certificate 0123abcd', done.stdout)
        page = self.rookery('provisioner', 'show', name).stdout
        self.assertIn(f'place=https://{HOST}:9999', page)
        self.assertIn(f'The lathe at https://{HOST}:9999 answers', page)
        self.assertIn(f'Node      {HOST}, whose feature offers it', page)
        # The host records what it is and what it offers, and says so.
        applied = json.loads(self.root('cat /etc/rookery/applied.json').stdout)
        self.assertEqual(applied['level'], 'node')
        self.assertEqual(applied['offers'][0]['feature'], 'depot/lathe')
        self.assertEqual(self.station.link().get('/v1/version')['type'], 'node')
        # Only the certificate rookery made went to the node, never its key.
        trusted = self.root('cat /etc/depot-lathe.crt').stdout
        self.assertTrue(trusted.startswith('-----BEGIN CERTIFICATE-----'))
        self.assertNotIn('PRIVATE KEY', self.root('cat /etc/rookery/*.json /etc/depot-lathe.crt').stdout)
        # Sessions run as the SSH user; nothing of a workstation is there.
        self.assertEqual(self.rookery('exec', HOST, '--', 'id', '-un').stdout.strip(), OPERATOR)
        for absent in ('id agent', 'test -e /etc/rookery/AGENT.md', 'test -e /etc/sudoers.d/90-rookery-agent',
                       'command -v Xvnc'):
            self.assertNotEqual(self.root(absent, check=False).returncode, 0, absent)
        listed = self.rookery('action', 'list', HOST).stdout
        for action in ('status', 'update', 'secrets'):
            self.assertRegex(listed, rf'(?m)^{action}\s+')
        # No rookery server's container here, so no upgrade of one.
        self.assertNotRegex(listed, r'(?m)^upgrade-server\s+')
        # The list names the node it lives on.
        self.assertRegex(self.rookery('provisioner', 'list').stdout, rf'(?m)^{name}\s+depot/box\s+{HOST}\s')
        self.assertIn('lathe', self.rookery('run', HOST, 'status').stdout)
        # SSH takes keys only, and leaves root none, as rookery logs in as the operator.
        sshd = self.root('cat /etc/ssh/sshd_config.d/00-rookery.conf').stdout
        self.assertIn('PasswordAuthentication no', sshd)
        self.assertIn('PermitRootLogin no', sshd)

    def test_3_applying_again_registers_nothing_new(self):
        done = self.rookery('apply', HOST, timeout=900)
        self.assertNotIn('Registered the provisioner', done.stdout)
        self.assertEqual(self.provisioners().count(f'{HOST}-depot-box'), 1)

    def test_4_a_provisioner_that_holds_a_deployment_stays_with_its_node(self):
        name = f'{HOST}-depot-box'
        self.rookery('resource', 'create', 'crate', '--provisioner', name, '--spec', '-', input='one crate\n')
        self.profile([])
        refused = self.rookery('apply', HOST, check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn(f'offers the provisioner {name} through depot/lathe, which still holds', refused.stderr)
        removed = self.station.remove_host(check=False)
        self.assertNotEqual(removed.returncode, 0)
        self.assertIn(f'is the node of the provisioner {name}', removed.stderr)
        self.assertIn(name, self.provisioners())

    def test_5_a_deployments_endpoints_are_reached_through_the_nodes_controller(self):
        echo = ('import socket, threading\n'
                's = socket.create_server(("0.0.0.0", ECHO))\n'
                'def serve(c):\n'
                '    while True:\n'
                '        data = c.recv(65536)\n'
                '        if not data: break\n'
                '        c.sendall(data)\n'
                '    c.shutdown(socket.SHUT_WR)\n'
                'while True: threading.Thread(target=serve, args=(s.accept()[0],)).start()\n').replace(
                    'ECHO', str(ECHO))
        self.root(f'setsid python3 -c {shell_quote(echo)} rookery-e2e-echo </dev/null >/dev/null 2>&1 &')
        serve = Serve(self.station)
        socket = serve.socket()
        try:
            resources = serve.call('resources.list')
            [crate] = [resource for resource in resources if resource['name'] == 'crate']
            seen = serve.call('resources.get', {'id': crate['id']})
            self.assertEqual([endpoint['name'] for endpoint in seen['endpoints']], ['db', 'lan', 'far'], seen)
            node = (self.station.rookery_dir / 'plugins/example/depot/rookery/bin/provision.node').read_text()
            self.assertEqual(node, HOST)
            data = bytes(range(256)) * 4096

            def through(endpoint):
                back = socket.through('resources.connect', {'id': crate['id'], 'endpoint': endpoint}, data)
                return None if isinstance(back, dict) else back
            for endpoint in ('db', 'lan'):
                back = wait_for(f'the endpoint {endpoint}', lambda endpoint=endpoint: through(endpoint), timeout=30)
                self.assertEqual(back, data, endpoint)
            # Off the node's networks it is refused.
            refused = socket.through('resources.connect', {'id': crate['id'], 'endpoint': 'far'}, data)
            self.assertIn('error', refused)
        finally:
            socket.close()
            self.root('pkill -f rookery-e2e-echo', check=False)

    def test_6_without_its_feature_the_provisioner_goes(self):
        name = f'{HOST}-depot-box'
        self.rookery('resource', 'remove', 'crate', '-y')
        done = self.rookery('apply', HOST, timeout=900)
        self.assertIn(f'✓ Forgot the provisioner {name}', done.stdout)
        self.assertNotIn(name, self.provisioners())
        self.assertNotIn('offers', json.loads(self.root('cat /etc/rookery/applied.json').stdout))


if __name__ == '__main__':
    unittest.main()
