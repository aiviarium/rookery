"""Updating a plugin from a registry, telling that the host is behind, and
bringing it up to date again without losing what was made on it: from this
machine, through rookery serve with a terminal client, and for a plugin that
comes with rookery and changed with it.

Update and ShippedChanged use the extended state of the host
(harness.extended()), which applied `up` 1.0.0 from the registry and the
shipped `shipit`; Update leaves `up` at 2.0.0 and the host current, which
ShippedChanged starts from. UpdateServed uses the shared state of rookery serve
(harness.served_extended()), which applied `served` 1.0.0."""
import json
import unittest

from harness import (AGENT, HOST, SCRATCH, Workstation, extended, light_vars, needs_e2e, plugin_registry,
                     plugin_version, random_text, served_extended, unb64, wait_for)

PACKAGE = 'example/up'


def version_of(applied, plugin):
    """The version of a plugin the host says it was applied with."""
    return next(found['version'] for found in applied['plugins'] if found['id'] == plugin)


class KeepsState:
    """What the agent made on the host before an update, checked afterwards."""

    def __init__(self, topic):
        self.topic = topic

    def plant(self, station, run):
        """Make the things with `run`, a command on the host as the agent."""
        self.mark = random_text('kept')
        run(f'echo {self.mark} > ~/kept-by-agent.txt')
        run(f'mkdir -p /srv/agent/workspaces/kept && echo {self.mark} > /srv/agent/workspaces/kept/notes.md')
        run(f"box publish {self.topic} '{json.dumps({'mark': self.mark})}'")

    def check(self, test, run):
        test.assertEqual(run('cat ~/kept-by-agent.txt').strip(), self.mark)
        test.assertEqual(run('cat /srv/agent/workspaces/kept/notes.md').strip(), self.mark)


@needs_e2e
class Update(unittest.TestCase):
    """From this machine, with the plugin from the registry."""

    @classmethod
    def setUpClass(cls):
        cls.station = extended().station
        cls.registry = plugin_registry()
        cls.link = cls.station.link()

    @classmethod
    def tearDownClass(cls):
        cls.station.remove_host('alpha', check=False)
        cls.station.rookery('host', 'set', HOST, 'secret.up-key=', check=False)
        cls.station.rookery('secret', 'remove', 'up-key', '-y', check=False)

    def run_as_agent(self, command):
        return self.station.rookery('exec', HOST, '--', 'bash', '-c', command).stdout

    def applied(self):
        return self.link.get('/v1/version').get('applied')

    def test_an_update_reaches_the_host_and_keeps_what_was_there(self):
        station = self.station
        # What the host says it was applied from.
        applied = self.applied()
        self.assertEqual(version_of(applied, 'up'), '1.0.0')
        self.assertEqual(station.rookery('run', HOST, 'up:old').stdout.strip(), 'old task')

        # What the agent has before the update: files, a topic, a sent secret
        # and a session that was kept.
        kept = KeepsState('up/notes')
        kept.plant(station, self.run_as_agent)
        secret = random_text('up-secret')
        station.rookery('secret', 'set', 'up-key', input=secret + '\n')
        station.rookery('host', 'set', HOST, 'secret.up-key=~/.config/e2e/up.key:0600')
        station.rookery('run', HOST, 'secrets')
        session = station.rookery('exec', HOST, '--keep', '--', 'echo', f'session {kept.mark}')
        self.assertIn(kept.mark, session.stdout)

        # A newer version in the registry.
        self.registry.publish(plugin_version('2.0.0'))
        checked = station.rookery('plugin', 'update', '--check').stdout
        self.assertRegex(checked, r'(?m)^up\s+1\.0\.0\s+2\.0\.0\s*$')
        updated = station.rookery('plugin', 'update', 'up').output
        self.assertIn('✓ example/up 1.0.0 → 2.0.0', updated)
        self.assertIn(f'{HOST} needs applying: up 1.0.0 → 2.0.0', updated)
        plugins = json.loads(station.rookery('plugin', 'list', '-o', 'json').stdout)['plugins']
        self.assertIn(('up', PACKAGE, '2.0.0'), [(p['id'], p['package'], p['version']) for p in plugins])

        # The overview says so too.
        overview = station.terminal('ui', HOST, cols=160)
        try:
            overview.expect(r'Applied before these changed: up 1\.0\.0 → 2\.0\.0', timeout=60)
        finally:
            overview.close()

        # A roll-out that fails at its first host leaves the next as it was.
        alpha = station.read_profile()
        alpha['ansible_vars'] = light_vars(['up/missing'])
        station.write_profile(alpha, name='alpha')
        stopped = station.rookery('rollout', check=False, timeout=900)
        self.assertEqual(stopped.returncode, 1, stopped.stdout)
        self.assertRegex(stopped.stdout, r'(?m)^alpha: failed: applying failed')
        self.assertRegex(stopped.stdout, rf'(?m)^{HOST}: not started$')
        self.assertEqual(version_of(self.applied(), 'up'), '1.0.0')
        station.remove_host('alpha')

        # Brought up to date.
        rolled = station.rookery('rollout', timeout=900).stdout
        self.assertIn(f'▸ {HOST}: applying (up 1.0.0 → 2.0.0)', rolled)
        self.assertIn(f'✓ {HOST} is up to date', rolled)
        self.assertEqual(version_of(self.applied(), 'up'), '2.0.0')
        self.assertIn(f'✓ {HOST} is current', station.rookery('rollout').stdout)

        # What changed is there, and what went is gone.
        self.assertEqual(station.rookery('run', HOST, 'up:hello').stdout.strip(), 'hello from 2.0.0')
        self.assertEqual(station.rookery('run', HOST, 'up:added').stdout.strip(), 'added task')
        tasks = station.rookery('action', 'list', HOST).stdout
        self.assertNotIn('up:old', tasks)
        services = json.loads(station.root_on_host('cat /etc/rookery/services.json').stdout)
        ours = [service['run'] for service in services if 'box serve up/echo' in service['run']]
        self.assertEqual(len(ours), 1, services)
        self.assertIn('2.0.0', ours[0])
        script = station.root_on_host('cat /usr/local/lib/rookery/services/up-echo.sh').stdout
        self.assertIn('rookery-e2e-echo 2.0.0', script)
        self.assertEqual(station.root_on_host('cat /usr/local/lib/rookery-up/two').stdout.strip(), '2.0.0')

        # And what the agent had is still there.
        kept.check(self, self.run_as_agent)
        self.assertEqual(self.run_as_agent('cat ~/.config/e2e/up.key').strip(), secret)
        sessions = station.rookery('session', 'list', HOST).stdout
        self.assertIn(f'session {kept.mark}', sessions)
        notes = self.link.get('/v1/channel?topic=up/notes')
        messages = notes['messages'] if isinstance(notes, dict) else notes
        self.assertIn(kept.mark, [message['body'].get('mark') for message in messages])


@needs_e2e
class ShippedChanged(unittest.TestCase):
    """A plugin that comes with rookery changes with rookery, keeping its version.
    The roll-out that applies the change is asked to reboot the host too,
    which says it needs one while a session runs there."""

    @classmethod
    def setUpClass(cls):
        cls.station = station = extended().station
        cls.link = station.link()
        cls.current = station.rookery('rollout').stdout
        cls.refused = station.rookery('plugin', 'update', 'shipit', check=False).stderr
        # The same version, another task.
        source = SCRATCH / 'source-shipped/plugins/shipit/rookery/rookery.json'
        manifest = json.loads(source.read_text())
        manifest['features'][0]['actions'][0]['run'] = 'echo "hello from the new rookery"'
        source.write_text(json.dumps(manifest))
        station.root_on_host('touch /var/run/reboot-required')
        sleeper = cls.link.post('/v1/sessions', {'kind': 'process', 'title': 'busy', 'command': ['sleep', '600']})
        try:
            cls.rolled = station.rookery('rollout', '--reboot', check=False, timeout=900)
        finally:
            cls.link.post(f'/v1/sessions/{sleeper["id"]}/signal', {'signal': 'KILL'})
            station.root_on_host('rm -f /var/run/reboot-required')

    def test_the_host_is_behind_until_applied(self):
        self.assertIn(f'✓ {HOST} is current', self.current)
        self.assertIn('comes with rookery', self.refused)
        self.assertIn(f'▸ {HOST}: applying (shipit 1.0.0 (changed))', self.rolled.stdout)
        self.assertEqual(self.station.rookery('run', HOST, 'shipit:hello').stdout.strip(), 'hello from the new rookery')

    def test_a_reboot_waits_for_running_sessions(self):
        self.assertEqual(self.rolled.returncode, 1, self.rolled.stdout)
        self.assertRegex(self.rolled.stdout, rf'(?m)^{HOST}: failed: needs a reboot, but 1 session is running there')


@needs_e2e
class UpdateServed(unittest.TestCase):
    """Through rookery serve, used from a terminal client."""

    @classmethod
    def setUpClass(cls):
        applied = served_extended()
        cls.serve = applied.serve
        cls.registry = plugin_registry()
        # The job that applied the plugin ended well.
        ended = cls.serve.call('GET', f'/v1/jobs/{applied.job["id"]}')
        assert ended['exit_code'] == 0, unb64(
            cls.serve.call('GET', f'/v1/jobs/{applied.job["id"]}/output')['data_b64'])[-3000:]
        cls.station = Workstation('update-client', ssh_key=False)
        cls.station.rookery('login', cls.serve.url, '--token-stdin', input=cls.serve.token)

    def host(self):
        return self.serve.call('GET', f'/v1/hosts/{HOST}')

    def test_an_update_on_the_server_is_rolled_out(self):
        station = self.station
        self.assertNotIn('behind', self.host())
        mark = random_text('kv')
        self.serve.call('PUT', '/v1/kv/update/mark', {'mark': mark})
        kept = KeepsState('served/notes')

        def run(command):
            return station.rookery('exec', HOST, '--', 'bash', '-c', command).stdout
        kept.plant(station, run)

        self.registry.publish(plugin_version('2.0.0', 'served'))
        # The tab of the terminal client asks the server. It lists every plugin
        # with its features before the row that adds one, so it is tall.
        home = station.terminal('ui', cols=160, rows=46)
        try:
            home.expect(HOST)
            home.send('6')
            home.expect('Add plugin')
            home.send('c')
            home.expect(r'update available: 1\.0\.0 → 2\.0\.0', timeout=60)
        finally:
            home.close()
        updated = station.rookery('plugin', 'update').output
        self.assertIn('✓ example/served 1.0.0 → 2.0.0', updated)
        self.assertIn(f'{HOST} needs applying: served 1.0.0 → 2.0.0', updated)
        behind = self.host()['behind']
        self.assertEqual(behind['changes'], [{'id': 'served', 'from': '1.0.0', 'to': '2.0.0'}])
        listed = self.serve.call('GET', '/v1/hosts')
        self.assertTrue(any(host['name'] == HOST and 'behind' in host for host in listed))

        rolled = station.rookery('rollout', timeout=900)
        self.assertIn(f'✓ {HOST} is up to date', rolled.stdout)
        self.assertRegex(rolled.stdout, rf'(?m)^{HOST}: up to date$')
        jobs = self.serve.call('GET', '/v1/jobs')
        rollout = next(job for job in jobs if job['action'] == 'rollout')
        self.assertEqual(rollout['report']['hosts'][0]['state'], 'done')
        self.assertTrue(rollout['report']['ok'])
        # What the server last heard from the host is kept a few seconds.
        wait_for('the server hearing that the host is current', lambda: 'behind' not in self.host(), timeout=30)
        self.assertEqual(station.rookery('run', HOST, 'served:hello').stdout.strip(), 'hello from 2.0.0')
        kept.check(self, run)
        self.assertEqual(self.serve.call('GET', '/v1/kv/update/mark')['value'], {'mark': mark})
        self.assertEqual(run('id -un').strip(), AGENT)
