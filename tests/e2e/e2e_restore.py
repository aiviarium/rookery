"""An installation restored from its database and its key file alone.

A rookery serve keeps its state in its PostgreSQL database, with its secrets and
key ring sealed under its key. The test gives it the test host, a finished job, a
secret, a setting, a key of the ring and an access token, stops it and wipes its
configuration, data and runtime directories. A copy of the database is all that
is kept, with the key file, and a rookery serve started on them is the same
installation: the host, the token, rookery's key, the secret, the setting and the
job with its log are there. Without the key file, it serves, and refuses what is
sealed.

It runs on `studio` after the other tests of rookery serve and applies nothing:
the host is the one provisioned() set up, and the server is its own."""
import os
import shutil
import subprocess
import unittest
import urllib.parse

from harness import (AGENT, HOST, OPERATOR, Serve, Workstation, _provisioned, hand_over_operator_key, light_vars,
                     make_database, needs_e2e, provisioned, random_text, wait_for)


def wipe(station):
    """Remove everything under the station's configuration, data and runtime directories."""
    for folder in (station.config, station.data, station.run_dir):
        for entry in folder.iterdir():
            shutil.rmtree(entry) if entry.is_dir() and not entry.is_symlink() else entry.unlink()


def public_of(key_file):
    """The public half of a private key file, as the two fields of an authorized_keys line."""
    shown = subprocess.run(['ssh-keygen', '-y', '-f', str(key_file)], capture_output=True, text=True, check=True)
    return shown.stdout.split()[:2]


def pg(url):
    """The libpq arguments and environment of a database URL."""
    parts = urllib.parse.urlsplit(url)
    args = ['-h', parts.hostname, '-p', str(parts.port or 5432), '-U', parts.username, parts.path.lstrip('/')]
    return args, {**os.environ, 'PGPASSWORD': parts.password or ''}


def dump(url):
    args, env = pg(url)
    return subprocess.run(['pg_dump', '--no-owner', *args], env=env, capture_output=True, check=True,
                          timeout=120).stdout


def restore(text, url):
    args, env = pg(url)
    subprocess.run(['psql', '-q', '-v', 'ON_ERROR_STOP=1', *args], env=env, input=text, capture_output=True,
                   check=True, timeout=120)


@needs_e2e
class Restore(unittest.TestCase):
    """The tests look at the restored server, set up once, in the order of their names."""

    @classmethod
    def setUpClass(cls):
        cls.station = station = Workstation('restore', ssh_key=False, trust=False)
        cls.worker = provisioned()
        cls.own_public = station.own_public()
        cls.give_the_host_the_keys()
        key = station.own_key_file()
        wait_for('the operator taking the key of the server', lambda: station.ssh(
            OPERATOR, 'true', key=key, check=False).returncode == 0)
        cls.fill(Serve(station))
        station.stop_server()
        cls.dumped = dump(station.database)
        # A database of its own, as one restored from a backup, and the key file: all that is kept.
        restored = make_database('restored')
        restore(cls.dumped, restored)
        cls.keyless = make_database('keyless')
        restore(cls.dumped, cls.keyless)
        wipe(station)
        station.env['ROOKERY_DATABASE_URL'] = station.database = restored
        station.start_server()
        cls.serve = Serve(station)

    @classmethod
    def give_the_host_the_keys(cls):
        """The operator takes the server's key the way the first one did, besides the others'."""
        cls.others = [cls.worker.public]
        if 'serve' in _provisioned:
            cls.others.append(_provisioned['serve'].station.own_public())
        hand_over_operator_key(*cls.others, cls.own_public)

    @classmethod
    def fill(cls, serve):
        """Put a host, a job, a secret, a setting and a key in the first server, and note them."""
        host = cls.worker.read_profile()
        host['access']['auth'] = 'rookery'
        host['ansible_vars'] = light_vars()
        for key in ('secrets', 'registries'):
            host.pop(key, None)
        serve.call('hosts.save', {'host': HOST, 'profile': host})
        wait_for('the host answering through the server', lambda: [
            known for known in serve.call('hosts.list')['hosts'] if known['name'] == HOST and known['reachable']])
        cls.job = serve.call('hosts.act', {'host': HOST, 'action': 'status'})['job']
        cls.finished, cls.output = serve.wait_job(cls.job, timeout=120)
        assert cls.output.strip(), 'the job wrote nothing'
        cls.secret = random_text('restored-secret')
        cls.secret_value = random_text('value')
        serve.call('installation.secrets.set', {'name': cls.secret, 'value': cls.secret_value})
        serve.call('installation.settings.set', {'name': 'retention_days', 'value': '37'})
        cls.ring_key = random_text('ring')
        serve.call('keys.add', {'name': cls.ring_key})
        cls.keys = {key['name']: key['public'].split()[:2] for key in serve.call('keys.list')['keys']}
        cls.token = serve.token
        cls.own_file_public = public_of(cls.station.own_key)

    @classmethod
    def tearDownClass(cls):
        hand_over_operator_key(*cls.others)

    def test_1_the_secret_value_and_the_keys_are_sealed_in_the_database(self):
        self.assertTrue(self.secret_value.encode() not in self.dumped, 'the secret is in the database as it is')
        self.assertTrue(b'PRIVATE KEY' not in self.dumped, 'a key of the ring is in the database as it is')

    def test_2_the_host_is_listed_and_reachable(self):
        found = wait_for('the restored server reaching the host', lambda: [
            known for known in self.serve.call('hosts.list')['hosts'] if known['name'] == HOST and known['reachable']])
        self.assertTrue(found[0]['version'])
        self.assertEqual(self.serve.call('hosts.get', {'host': HOST})['address'], HOST)

    def test_3_the_token_and_the_key_of_rookery_are_the_same(self):
        # The token of before works, without showing it in a failure.
        status, _ = self.serve.rpc('hosts.list', token=self.token)
        self.assertEqual(status, 200)
        self.assertEqual(self.serve.rpc('hosts.list', token='rookery_pat_not-the-token')[0], 401)
        wait_for('rookery writing its key file', lambda: self.station.own_key.is_file())
        self.assertEqual(self.station.own_key.stat().st_mode & 0o777, 0o600)
        self.assertEqual(public_of(self.station.own_key), self.own_file_public)
        self.assertEqual(self.own_file_public, self.own_public.split()[:2])
        keys = {key['name']: key['public'].split()[:2] for key in self.serve.call('keys.list')['keys']}
        self.assertEqual(keys, self.keys)
        self.assertIn(self.ring_key, keys)
        self.assertEqual(keys['rookery'], self.own_file_public)

    def test_4_the_secret_and_the_setting_are_there(self):
        self.assertIn(self.secret, self.serve.call('installation.secrets.list')['names'])
        settings = {setting['key']: setting for setting in self.serve.call('installation.settings.list')}
        self.assertEqual(settings['retention_days']['value'], '37')
        # The secret's value opens too: it reaches the host the way a server's secrets do.
        profile = self.serve.call('hosts.profile', {'host': HOST})['profile']
        profile['secrets'] = [{'name': self.secret, 'env': 'RESTORED_SECRET'}]
        self.serve.call('hosts.save', {'host': HOST, 'profile': profile})
        acted = self.serve.call('hosts.act', {'host': HOST, 'action': 'secrets'})
        if acted.get('job'):
            job, log = self.serve.wait_job(acted['job'])
            self.assertEqual(job['state'], 'succeeded', log)
        else:
            wait_for('the secrets action to end', lambda: self.serve.call(
                'hosts.sessions.get', {'host': HOST, 'terminal': acted['session']['id']}).get('exit_code') == 0)
        env = self.worker.root_on_host(f'cat /home/{AGENT}/.config/rookery/secrets.env').stdout
        self.assertIn(f'export RESTORED_SECRET={self.secret_value}', env)

    def test_5_the_old_job_is_listed_with_its_log(self):
        jobs = self.serve.call('jobs.list', {'subject': f'env://{HOST}'})['jobs']
        self.assertIn(self.job, [job['id'] for job in jobs])
        job = self.serve.call('jobs.get', {'id': self.job})
        self.assertEqual((job['params']['action'], job['state']), ('status', self.finished['state']))
        self.assertEqual(self.serve.log(self.job), self.output)

    def test_6_without_the_key_file_it_serves_and_refuses_what_is_sealed(self):
        self.station.stop_server()
        wipe(self.station)
        env = {**self.station.env, 'ROOKERY_DATABASE_URL': self.keyless}
        del env['ROOKERY_SECRET_KEY_FILE']
        self.station.env = env
        self.station.start_server()
        serve = Serve(self.station)
        self.assertIn(HOST, [host['name'] for host in serve.call('hosts.list')['hosts']])
        self.assertEqual(serve.refused('secrets.get', {'plugin': 'rookery', 'name': self.secret}), 'UNAVAILABLE')
        self.assertFalse((self.station.data / 'rookery/keys/rookery').exists())
