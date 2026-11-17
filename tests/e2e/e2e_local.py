"""What rookery does on the workstation alone: its key, the cloud seed, the
inventory and the API documents."""
import json
import unittest

from harness import HOST, OPERATOR, Workstation, needs_e2e, profile, provisioned


@needs_e2e
class Key(unittest.TestCase):
    def test_rookery_makes_its_own_key_once(self):
        station = Workstation('keys', ssh_key=False, trust=False)
        self.assertFalse(station.own_key.exists())
        first = station.own_public()
        self.assertRegex(first, r'^ssh-ed25519 \S+ rookery@\S+$')
        self.assertEqual(station.own_public(), first)
        listed = json.loads(station.rookery('key', 'list', '-o', 'json').stdout)['keys']
        self.assertEqual([(key['name'], key['public']) for key in listed], [('rookery', first)])
        self.assertRegex(listed[0]['fingerprint'], r'^SHA256:\S+$')
        self.assertRegex(station.rookery('key', 'list').stdout, r'(?m)^rookery\s+SHA256:\S+')
        # The key lives in the store; it is a file only once ssh needs one.
        self.assertFalse(station.own_key.exists())
        station.write_profile(profile(hostname='attic', access={'address': '192.0.2.61'}), name='attic')
        inventory = json.loads(station.rookery('host', 'inventory', 'attic').stdout)['all']['hosts']['attic']
        self.assertIn(f'IdentityFile={station.own_key}', inventory['ansible_ssh_common_args'])
        self.assertEqual(station.own_key.stat().st_mode & 0o777, 0o600)
        self.assertEqual(station.own_key.with_suffix('.pub').read_text().strip(), first)
        # The store keeps both halves, so a lost file comes back as it was.
        station.own_key.with_suffix('.pub').unlink()
        station.rookery('host', 'inventory', 'attic')
        self.assertEqual(station.own_key.with_suffix('.pub').read_text().strip(), first)
        self.assertEqual(station.own_public(), first)


@needs_e2e
class Seed(unittest.TestCase):
    def test_the_cloud_seed_carries_the_operators_keys(self):
        station = Workstation('seeds')
        own = station.own_public()
        station.write_profile(profile(
            hostname='cloud-1', operator_ssh_public_keys=[station.public], access={},
            # What F5 or mkpasswd would store; nobody logs in with it.
            operator_password_hash='$6$e2e$' + 'x' * 86,
            release_url='https://192.0.2.30/rookery-release.tar.gz'), name='cloud-1')
        written = station.root / 'user-data'
        done = station.rookery('host', 'seed', 'cloud-1', '--file', str(written), timeout=300)
        seeds = [written]
        self.assertTrue(written.exists(), done.stdout + done.stderr)
        seed = seeds[0].read_text()
        self.assertTrue(seed.startswith('#cloud-config'), seed[:200])
        self.assertIn(station.public.split()[1], seed)
        self.assertIn(own.split()[1], seed)
        self.assertIn(OPERATOR, seed)
        self.assertEqual(seeds[0].stat().st_mode & 0o077, 0)


def listed(station):
    """The hosts rookery lists, by name and address."""
    return [(host['name'], host['address']) for host in json.loads(station.rookery('ls', '-o', 'json').stdout)]


@needs_e2e
class Profiles(unittest.TestCase):
    """Profiles live in rookery's store, and come in and out as JSON."""

    def test_a_profile_goes_out_and_in_and_is_removed_with_what_is_kept_of_it(self):
        station = Workstation('profiles', ssh_key=False, trust=False)
        kept = profile(hostname='depot', access={'address': '192.0.2.50', 'user': OPERATOR})
        station.write_profile(kept, name='depot')
        self.assertEqual(station.read_profile('depot'), kept)
        self.assertIn(('depot', '192.0.2.50'), listed(station))
        self.assertFalse((station.rookery_dir / 'hosts').exists())
        # What is not a profile is refused, and the saved one stays.
        refused = station.rookery('host', 'import', 'depot', '-', input='[]', check=False)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(station.read_profile('depot'), kept)
        # Passwords go to a terminal, or to a pipe only when asked to.
        piped = station.rookery('host', 'credentials', 'depot', check=False)
        self.assertNotEqual(piped.returncode, 0)
        self.assertIn('--show', piped.stderr)
        self.assertIn('no passwords', station.rookery('host', 'credentials', 'depot', '--show').stderr)
        station.remove_host('depot')
        self.assertNotIn('depot', station.rookery('ls').stdout)
        self.assertNotEqual(station.rookery('host', 'export', 'depot', check=False).returncode, 0)

    def test_the_hosts_of_an_older_rookery_move_into_the_store(self):
        station = Workstation('older-hosts', ssh_key=False, trust=False)
        hosts = station.rookery_dir / 'hosts'
        hosts.mkdir(parents=True, mode=0o700)
        # A profile of rookery 0.4, which kept how it is reached under builder.
        old = profile(hostname='attic', builder={'ssh_host': '192.0.2.60', 'shipped_plugins': True})
        old.pop('access')
        (hosts / 'attic.json').write_text(json.dumps(old))
        (hosts / 'attic.credentials.json').write_text(json.dumps({OPERATOR: 'made-up-password'}))
        (hosts / 'attic.instance.lock').write_text('')
        mirrored = station.data / 'rookery/served/http---192-0-2-7-7777/hosts'
        mirrored.mkdir(parents=True)
        (mirrored / 'depot.json').write_text('{}')
        moved = station.rookery('ls', '-o', 'json')
        self.assertIn(('attic', '192.0.2.60'), [(host['name'], host['address']) for host in json.loads(moved.stdout)])
        self.assertIn('moved 1 host into rookery', moved.stderr)
        self.assertFalse(hosts.exists())
        self.assertTrue((station.rookery_dir / 'migrated/hosts/attic.json').is_file())
        self.assertFalse((station.data / 'rookery/served').exists())
        # It is kept in the layout of now; without a key file it logs in as it did.
        self.assertEqual(station.read_profile('attic'), {
            **old, 'builder': {'shipped_plugins': True}, 'access': {'address': '192.0.2.60', 'auth': 'ssh'}})
        shown = json.loads(station.rookery('host', 'credentials', 'attic', '-o', 'json').stdout)
        self.assertEqual(shown['credentials'], {OPERATOR: 'made-up-password'})
        # Once is enough.
        self.assertNotIn('moved', station.rookery('ls').stderr)


@needs_e2e
class Screens(unittest.TestCase):
    """The screens start with a server in rookery itself, show the host and what
    rookeryd says of it, and quit."""

    @classmethod
    def setUpClass(cls):
        cls.station = provisioned()

    def test_the_home_screen_lists_the_host(self):
        terminal = self.station.terminal('ui', cols=120, rows=30)
        try:
            terminal.expect(HOST)
            terminal.send('q')
            self.assertEqual(terminal.wait(), 0)
        finally:
            terminal.close()

    def test_the_overview_shows_the_hosts_tasks(self):
        terminal = self.station.terminal('ui', HOST, cols=120, rows=46)
        try:
            # The rows of the tasks come from rookeryd on the host.
            terminal.expect('Secrets and registry logins')
            # q goes back to the hosts, and there it quits.
            terminal.send('q')
            terminal.expect('New host')
            terminal.send('q')
            self.assertEqual(terminal.wait(), 0)
        finally:
            terminal.close()


@needs_e2e
class Listings(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.station = provisioned()

    def test_hosts_and_their_inventory(self):
        self.assertIn((HOST, HOST), listed(self.station))
        inventory = json.loads(self.station.rookery('host', 'inventory', HOST).stdout)
        target = inventory['all']['hosts'][HOST]
        self.assertEqual((target['ansible_host'], target['ansible_user']), (HOST, OPERATOR))
        self.assertIn(f'IdentityFile={self.station.own_key}', target['ansible_ssh_common_args'])
        self.assertIn('HostKeyAlias=', target['ansible_ssh_common_args'])

    def test_the_settings_say_where_things_are(self):
        shown = self.station.rookery('config', 'list').stdout
        self.assertRegex(shown, r'(?m)^  retention_days +\(not set\)$')
        # The client's own settings are listed apart, the token never shown.
        self.assertRegex(shown, r'(?m)^This rookery, on this machine\n  server +\(not set\)$')
        store = self.station.rookery('server', 'store', 'status').stdout
        self.assertIn(f"Store      SQLite file {self.station.data / 'rookery/rookery.db'}", store)

    def test_the_api_documents(self):
        operations = json.loads(self.station.rookery('server', 'api', 'operations').stdout)
        self.assertTrue(operations)
        openapi = json.loads(self.station.rookery('server', 'api', 'openapi').stdout)
        self.assertIn('/v1/hosts', openapi['paths'])
        asyncapi = json.loads(self.station.rookery('server', 'api', 'asyncapi').stdout)
        self.assertTrue(asyncapi.get('asyncapi'))
        self.assertRegex(self.station.rookery('--version').stdout, r'^rookery \d+\.\d+\.\d+')
