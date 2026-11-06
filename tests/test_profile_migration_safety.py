"""Refusal boundaries for actual profile-migration E2E; no live VM mutation."""
import importlib.util
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

TOOLS=Path(__file__).resolve().parents[1]/'tools'
sys.path.insert(0,str(TOOLS))
import lab_profile_migration as guest
spec=importlib.util.spec_from_file_location('migration_runner',TOOLS/'run-profile-migration-e2e.py')
runner=importlib.util.module_from_spec(spec);spec.loader.exec_module(runner)


class Safety(unittest.TestCase):
    def test_configuration_must_be_exact_bounded_direct_target(self):
        self.assertEqual(guest.direct_config({'transport':'direct','port':3241}),{'transport':'direct','port':3241})
        for value in (None,{}, {'transport':'direct','port':True}, {'transport':'direct','port':'3241'},
                      {'transport':'direct','port':1023}, {'transport':'direct','port':65536},
                      {'transport':'ssh','port':3241}, {'transport':'direct','port':3241,'proof':'foreign'}):
            with self.subTest(value=value),self.assertRaises(RuntimeError):guest.direct_config(value)

    def test_nonroot_refuses_before_guest_or_receiver_read(self):
        with patch.object(guest.os,'geteuid',return_value=1000), \
             patch.object(Path,'read_text',side_effect=AssertionError('unexpected read')), \
             self.assertRaisesRegex(RuntimeError,'guest root'):
            guest.run({})

    def test_non_qemu_refuses_before_receiver_execution(self):
        with patch.object(guest.os,'geteuid',return_value=0), \
             patch.object(Path,'read_text',return_value='Physical machine'), \
             patch.object(guest,'snapshot',side_effect=AssertionError('unexpected receiver')), \
             self.assertRaisesRegex(RuntimeError,'QEMU'):
            guest.run({})

    def settings(self):
        return dict(peer='a'*32,current_sha256='b'*64,
                    before={'trust':{'identity':'fixed'},'boot_id':'same'},
                    configuration={'transport':'direct','port':3241})

    def test_unknown_installed_image_is_never_executed(self):
        with patch.object(guest,'snapshot',side_effect=RuntimeError('Unexpected installed receiver')), \
             patch.object(guest.subprocess,'run',side_effect=AssertionError('unexpected install')), \
             self.assertRaisesRegex(RuntimeError,'Unexpected installed receiver'):
            guest.restore(self.settings())

    def test_changed_trust_prevents_configuration_restore(self):
        with patch.object(guest,'snapshot',return_value={'trust':{'identity':'changed'},'boot_id':'same'}), \
             patch.object(guest,'regular',side_effect=AssertionError('unexpected config read')), \
             self.assertRaisesRegex(RuntimeError,'Trust material changed'):
            guest.restore(self.settings())

    def test_foreign_ssh_peer_prevents_overwriting_receiver(self):
        settings=self.settings()
        current={'transport':'ssh','port':3242,'proof':'not logged','peer':'c'*32}
        with patch.object(guest,'snapshot',return_value=settings['before']), \
             patch.object(guest,'regular',return_value=(json.dumps(current).encode(),{})), \
             patch.object(guest.subprocess,'run',side_effect=AssertionError('unexpected install')), \
             self.assertRaisesRegex(RuntimeError,'unrelated receiver'):
            guest.restore(settings)

    def test_tls_health_waits_for_authenticated_device_poll_without_invented_peer(self):
        before={'trust':{},'boot_id':'same','config_hash':'fixed'}
        settings=self.settings()|{'before':before,'transport':'direct'}
        with patch.object(guest,'snapshot',side_effect=[
                RuntimeError('Receiver is not in expected idle/session state'),before]) as observe, \
             patch.object(guest.time,'sleep'):
            self.assertEqual(guest.connected(settings),before)
        self.assertEqual(observe.call_count,2)
        self.assertTrue(all(call.args==('b'*64,True,None) for call in observe.call_args_list))

    def test_connected_identity_failure_is_not_retried_as_poll_delay(self):
        settings=self.settings()|{'transport':'ssh'}
        with patch.object(guest,'snapshot',side_effect=RuntimeError('Connected receiver belongs to another peer')) as observe, \
             patch.object(guest.time,'sleep',side_effect=AssertionError('unexpected retry')), \
             self.assertRaisesRegex(RuntimeError,'another peer'):
            guest.connected(settings)
        observe.assert_called_once_with('b'*64,True,'a'*32)

    def test_physical_and_idle_soak_emulators_are_refused(self):
        for serial in ('physical-serial','emulator-5566','emulator-5568 extra','emulator-'):
            with self.subTest(serial=serial),self.assertRaises(ValueError):runner.protect_android(serial)

    def test_other_vm_lock_does_not_authorize_live_soak_android(self):
        identity={'pid':123,'start_ticks':456,'command_sha256':'a'*64}
        value=json.dumps({'serial':'emulator-5568','runner_identity':identity})
        with patch.object(Path,'exists',return_value=True),patch.object(Path,'read_text',return_value=value), \
             patch.object(runner,'process_identity',return_value=identity), \
             self.assertRaisesRegex(ValueError,'live active soak'):
            runner.protect_android('emulator-5568')


if __name__=='__main__':unittest.main()
