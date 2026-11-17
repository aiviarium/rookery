"""rookery's controller alone, on the bare test host: a machine added by its
address is reachable and refuses what needs more, install-controller puts
rookeryd and box on it without the rest of the role, and sessions, tasks and
secrets then work there without a full apply.

It runs on `forge` after e2e_access and before e2e_own_key, with one playbook
run. At the end the controller's files are gone again, rookeryd is stopped and
the key it handed over is taken back, so e2e_own_key finds the host as a
provider hands it over. The container has no systemd: its init starts rookeryd
once the controller's files are there, as ci/host.sh says."""
import json
import unittest

from harness import (HOST, OPERATOR, Workstation, hand_over_operator_key, needs_e2e, provisioned, random_text,
                     shell_quote, wait_for)

# What the controller puts on the host, removed again at the end.
INSTALLED = ('/usr/local/bin/rookeryd /usr/local/bin/box /usr/local/bin/rookery-secrets '
             '/usr/local/lib/rookery /etc/rookery /var/lib/rookeryd /run/rookery')


def actions(station, host):
    """A host's actions by their names, as the server lists them."""
    return {action['name']: action for action in json.loads(station.rookery('action', 'list', host, '-o', 'json').stdout)}


@needs_e2e
class Controller(unittest.TestCase):
    """The tests build on each other, in the order of their names."""

    @classmethod
    def setUpClass(cls):
        cls.station = Workstation('controller', ssh_key=False)
        hand_over_operator_key(cls.station.own_public())
        key = cls.station.own_key_file()
        wait_for('the operator taking the key of rookery', lambda: cls.station.ssh(
            OPERATOR, 'true', key=key, check=False).returncode == 0, timeout=60)
        cls.added = cls.station.rookery('host', 'add', HOST, HOST, '--user', OPERATOR, input='')

    @classmethod
    def tearDownClass(cls):
        cls.root(f'pkill -x rookeryd; rm -rf {INSTALLED} /home/{OPERATOR}/.config/rookery', check=False)
        hand_over_operator_key()

    @classmethod
    def root(cls, command, check=True):
        return cls.station.ssh(OPERATOR, f'sudo -n sh -c {shell_quote(command)}', key=cls.station.own_key,
                               check=check)

    def rookery(self, *args, **kwargs):
        kwargs.setdefault('input', '')
        kwargs.setdefault('timeout', 120)
        return self.station.rookery(*args, **kwargs)

    def test_1_a_machine_added_by_its_address_is_only_logged_in_to(self):
        self.assertIn(f'Added {HOST} as a host of the type ssh', self.added.stdout)
        self.assertIn(f'{HOST} takes the key rookery already', self.added.stdout)
        self.assertEqual(self.station.read_profile(), {
            'type': 'ssh', 'access': {'address': HOST, 'user': OPERATOR, 'auth': 'rookery'}})
        self.assertEqual(self.rookery('ssh', HOST, '--', 'id -un').stdout.strip(), OPERATOR)
        listed = actions(self.station, HOST)
        self.assertEqual(listed['install-controller']['label'], 'Install the controller')
        # What needs more than a login is refused, and says what changes the type.
        refused = self.rookery('apply', HOST, check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('neither a node nor a workstation', refused.stderr)
        refused = self.rookery('exec', HOST, '--', 'id', check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn(f'rookery run {HOST} install-controller', refused.stderr)
        self.assertNotEqual(self.root('test -e /usr/local/bin/rookeryd', check=False).returncode, 0)

    def test_2_installing_the_controller_puts_rookeryd_and_box_there_alone(self):
        done = self.rookery('run', HOST, 'install-controller', '-y', timeout=600)
        self.assertIn(f'{HOST} is a managed host now', done.stdout)
        self.assertIn(f"rookery's controller answers on {HOST}", done.stdout)
        self.assertEqual(self.station.read_profile()['type'], 'managed')
        # The role's record calls the type its level.
        applied = json.loads(self.root('cat /etc/rookery/applied.json').stdout)
        self.assertEqual((applied['level'], applied['plugins']), ('controller', []))
        version = self.station.link().get('/v1/version')
        self.assertEqual(version['type'], 'managed')
        # Nothing of a workstation: no agent account, desktop or role record.
        for absent in ('id agent', 'test -e /etc/rookery/AGENT.md', 'test -e /var/lib/rookery/status.json',
                       'test -e /etc/sudoers.d/90-rookery-agent', 'command -v Xvnc'):
            self.assertNotEqual(self.root(absent, check=False).returncode, 0, absent)

    def test_3_sessions_and_tasks_run_as_the_ssh_user(self):
        self.assertEqual(self.rookery('exec', HOST, '--', 'id', '-un').stdout.strip(), OPERATOR)
        self.assertEqual(self.rookery('exec', HOST, '--user', 'root', '--', 'id', '-un').stdout.strip(), 'root')
        listed = actions(self.station, HOST)
        for name in ('status', 'update', 'secrets'):
            self.assertIn(name, listed)
        self.assertNotIn('smoke', listed)
        self.assertEqual(listed['install-controller']['label'], 'Update the controller')
        status = self.rookery('run', HOST, 'status')
        self.assertIn('Type        managed', status.stdout)
        marker = random_text('controller')
        self.rookery('exec', HOST, '--keep', '--', 'echo', marker)
        line = next(line for line in self.rookery('session', 'list', HOST).stdout.splitlines() if marker in line)
        self.assertEqual(self.rookery('session', 'logs', HOST, line.split()[0]).stdout.strip(), marker)

    def test_4_secrets_go_to_the_ssh_user(self):
        value = random_text('controller-secret')
        self.rookery('secret', 'set', 'controller-token', input=value + '\n')
        self.rookery('host', 'set', HOST, 'secret.controller-token=CONTROLLER_TOKEN')
        sent = self.rookery('run', HOST, 'secrets')
        self.assertNotIn(value, sent.stdout + sent.stderr)
        env = self.root(f'cat /home/{OPERATOR}/.config/rookery/secrets.env').stdout
        self.assertIn(f'export CONTROLLER_TOKEN={value}', env)


@needs_e2e
class OnAWorkstation(unittest.TestCase):
    """The controller alone is never put on a workstation rookery set up, though
    a profile saved here under another name says the host is less: on
    `studio`, as provisioned() applied it."""

    def test_a_workstation_added_again_as_ssh_keeps_its_controller(self):
        station = provisioned()
        files = 'cat /etc/rookery/applied.json /etc/rookery/rookeryd.json /etc/rookery/actions.json'
        before = station.root_on_host(files).stdout
        added = station.rookery('host', 'add', 'adopted', HOST, '--user', OPERATOR, '--type', 'ssh', input='')
        try:
            # rookery sees what the host is, and says how to adopt it.
            self.assertIn('adopted is a workstation; its profile here says ssh', added.stdout)
            self.assertIn('rookery host adopt adopted', added.stdout)
            listed = actions(station, 'adopted')
            self.assertIn('take its controller apart', listed['install-controller']['why'])
            refused = station.rookery('run', 'adopted', 'install-controller', '-y', check=False, timeout=600)
            self.assertNotEqual(refused.returncode, 0)
            self.assertIn('would take its controller apart', refused.stdout + refused.stderr)
            self.assertEqual(station.root_on_host(files).stdout, before)
            self.assertEqual(station.read_profile('adopted')['type'], 'ssh')
            # Adopted from the profile the host keeps, by the address it was added at.
            adopted = station.rookery('host', 'adopt', 'adopted')
            self.assertIn('Adopted adopted', adopted.stdout)
            profile = station.read_profile('adopted')
            self.assertNotIn('type', profile)
            saved = station.read_profile()
            for key in ('hostname', 'operator_ssh_public_keys', 'agent_ssh_public_keys', 'ansible_vars'):
                self.assertEqual(profile[key], saved[key], key)
            self.assertEqual(profile['access'], {'address': HOST, 'user': OPERATOR, 'auth': 'rookery'})
            self.assertEqual(station.root_on_host(files).stdout, before)
        finally:
            station.remove_host('adopted', check=False)
