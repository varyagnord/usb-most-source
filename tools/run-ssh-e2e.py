#!/usr/bin/env python3
"""Dedicated loopback OpenSSH fixtures; never changes system sshd or user credentials."""
from pathlib import Path
import subprocess, os, socket, time, json, pwd, uuid, tomllib, shutil, argparse

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--host-key-type', choices=('ed25519', 'rsa'), default='ed25519')
options=parser.parse_args()

root=Path(__file__).resolve().parents[1]
base=root/'build'/('ssh-fixture-'+uuid.uuid4().hex)
base.mkdir(mode=0o700,parents=True)
user=pwd.getpwuid(os.getuid()).pw_name
sshd=Path('/usr/sbin/sshd')
assert sshd.is_file()
def command(args,**kw): return subprocess.run(args,check=True,**kw)
for key in ('host','client'):
    key_type=options.host_key_type if key=='host' else 'ed25519'
    command(['ssh-keygen','-q','-t',key_type,'-N','','-f',str(base/key)])
command(['ssh-keygen','-q','-t','rsa','-b','2048','-N','','-f',str(base/'client-rsa')])
command(['ssh-keygen','-q','-t','ed25519','-N','fixture-passphrase','-f',str(base/'client-encrypted')])
shutil.copyfile(base/'client-rsa',base/'client-rsa-pem');(base/'client-rsa-pem').chmod(0o600)
command(['ssh-keygen','-q','-p','-m','PEM','-P','','-N','','-f',str(base/'client-rsa-pem')],stdout=subprocess.DEVNULL)
command(['openssl','pkcs8','-topk8','-nocrypt','-in',str(base/'client-rsa-pem'),'-out',str(base/'client-rsa-pkcs8')]);(base/'client-rsa-pkcs8').chmod(0o600)
shutil.copyfile(base/'client-rsa',base/'client-rsa-encrypted');(base/'client-rsa-encrypted').chmod(0o600)
command(['ssh-keygen','-q','-p','-P','','-N','fixture-passphrase','-f',str(base/'client-rsa-encrypted')],stdout=subprocess.DEVNULL)
command(['openssl','pkcs8','-topk8','-v2','aes-256-cbc','-passout','pass:fixture-passphrase','-in',str(base/'client-rsa-pem'),'-out',str(base/'client-rsa-pkcs8-encrypted')]);(base/'client-rsa-pkcs8-encrypted').chmod(0o600)
(base/'authorized_keys').write_bytes(b''.join((base/p).read_bytes() for p in ['client.pub','client-rsa.pub','client-encrypted.pub']));(base/'authorized_keys').chmod(0o600)
ports=[]
for _ in range(2):
    with socket.socket() as s:
        s.bind(('127.0.0.1',0));ports.append(s.getsockname()[1])
assert ports[0]!=ports[1]
servers=[]
try:
    for port,forward in zip(ports,('remote','no')):
        config=base/f'sshd-{port}.conf';pidfile=base/f'sshd-{port}.pid'
        config.write_text(f'''Port {port}
ListenAddress 127.0.0.1
HostKey {base/'host'}
PidFile {pidfile}
AuthorizedKeysFile {base/'authorized_keys'}
AllowUsers {user}
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM no
PermitRootLogin no
AllowTcpForwarding {forward}
GatewayPorts no
PermitListen 127.0.0.1:*
AllowAgentForwarding no
X11Forwarding no
PermitTTY no
LogLevel ERROR
''')
        command(['sudo','-n',str(sshd),'-t','-f',str(config)])
        log=(base/f'sshd-{port}.log').open('w')
        process=subprocess.Popen(['sudo','-n',str(sshd),'-D','-e','-f',str(config)],stdout=log,stderr=subprocess.STDOUT)
        servers.append((process,pidfile,config,log))
        for _ in range(100):
            if process.poll() is not None:raise RuntimeError('fixture sshd exited; inspect its isolated log')
            if pidfile.exists():break
            time.sleep(.05)
        else:raise RuntimeError('fixture sshd start timeout')
    env=os.environ.copy();env.setdefault('RUSTUP_TOOLCHAIN',tomllib.loads((root/'rust-toolchain.toml').read_text())['toolchain']['channel'])
    env.update(USB_MOST_FIXTURE_USER=user,USB_MOST_FIXTURE_PORT=str(ports[0]),USB_MOST_FIXTURE_DENIED_PORT=str(ports[1]),USB_MOST_FIXTURE_KEY=str(base/'client'),USB_MOST_FIXTURE_HOST_PUB=str(base/'host.pub'))
    env['USB_MOST_FIXTURE_KEY_DIRECTORY']=str(base)
    cargo=str(Path.home()/'.cargo/bin/cargo')
    command([cargo,'run','--locked','-p','usb-most-connection','--example','ssh_fixture'],cwd=root,env=env,timeout=180)
    (base/'result.json').write_text(json.dumps({'status':'passed','host_key_type':options.host_key_type,'scope':'actual Rust client/OpenSSH on dedicated loopback listeners; synthetic echo bridge; no USB devices'},indent=2)+'\n')
    print('PASS OpenSSH fixture results:',base/'result.json',flush=True)
finally:
    for process,pidfile,config,log in servers:
        if pidfile.exists():
            pid=int(pidfile.read_text().strip())
            args=Path(f'/proc/{pid}/cmdline')
            if args.exists():
                assert str(config).encode() in args.read_bytes(),'refuse to stop unrelated daemon'
                subprocess.run(['sudo','-n','kill','-TERM',str(pid)],check=False)
        try:process.wait(timeout=10)
        except subprocess.TimeoutExpired:process.terminate()
        log.close()
