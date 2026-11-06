#!/usr/bin/env python3
"""Apply loss/delay exclusively inside a new, empty network namespace."""
import os,pathlib,subprocess,sys,json
root=pathlib.Path(__file__).resolve().parents[1]
assert os.geteuid()==0,'run through sudo unshare --net'
assert os.readlink('/proc/self/ns/net')!=os.readlink('/proc/1/ns/net'),'refuse host network namespace'
links=json.loads(subprocess.check_output(['/usr/sbin/ip','-j','link','show']))
assert [x['ifname'] for x in links]==['lo'],'refuse namespace with other interfaces'
exe=pathlib.Path(sys.argv[1]).resolve();assert exe.is_relative_to(root/'build')
delay,jitter,loss=map(int,sys.argv[2:]);assert 0<=jitter<=delay<=500 and 0<=loss<=5
run=lambda *a:subprocess.run(a,check=True,capture_output=True,text=True)
run('/usr/sbin/ip','link','set','lo','up')
run('/usr/sbin/tc','qdisc','add','dev','lo','root','handle','1:','prio','bands','3')
try:
 run('/usr/sbin/tc','qdisc','add','dev','lo','parent','1:3','handle','30:','netem','delay',f'{delay}ms',f'{jitter}ms','loss',f'{loss}%','limit','10000')
 for field in ('dport','sport'):
  run('/usr/sbin/tc','filter','add','dev','lo','protocol','ip','parent','1:','prio','3','u32','match','ip',field,'42377','0xffff','flowid','1:3')
 env=os.environ.copy();env['USB_MOST_LAB_PROXY_PORT']='42377'
 subprocess.run([str(exe),'wan::tests::real_tcp_automatic_agent_with_delay','--exact','--nocapture','--test-threads=1'],env=env,check=True,timeout=180)
 print(f'PASS isolated TCP netem one-way={delay}ms jitter={jitter}ms loss={loss}%',flush=True)
finally:
 subprocess.run(['/usr/sbin/tc','qdisc','del','dev','lo','root'],capture_output=True)
