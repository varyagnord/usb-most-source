"""Read only the unique synthetic HID in an owned guest; no physical input."""
SCRIPT = r'''
from pathlib import Path
import collections,json,os,select,struct,time
path=None;until=time.monotonic()+ENUMERATION_SECONDS
while time.monotonic()<until:
 for name in Path('/sys/class/input').glob('event*'):
  if (name/'device/name').read_text().strip()=='USB Most Lab USB Most Synthetic Mouse':
   path=Path('/dev/input')/name.name;break
 if path:break
 time.sleep(.1)
assert path,'synthetic HID was not enumerated'
fd=os.open(path,os.O_RDONLY|os.O_NONBLOCK)
counts=collections.Counter();values=collections.defaultdict(set);started=time.monotonic();until=started+READ_SECONDS
syns=0
try:
 while time.monotonic()<until:
  ready,_,_=select.select([fd],[],[],.5)
  if not ready:continue
  data=os.read(fd,24*128);assert len(data)%24==0
  for sec,usec,kind,code,value in struct.iter_unpack('llHHi',data):
   assert not(kind==0 and code==3),'SYN_DROPPED: input reader overflowed'
   if kind==0 and code==0:syns+=1
   if kind in [1,2]:counts[f'{kind}:{code}']+=1;values[f'{kind}:{code}'].add(value)
finally:os.close(fd)
result={'counts':dict(counts),'values':{k:sorted(v) for k,v in values.items()},
 'sync_reports':syns,'elapsed_seconds':round(time.monotonic()-started,3)}
print(json.dumps(result),flush=True)
for code in [272,273,274,275,276]:assert values[f'1:{code}']=={0,1},f'missing button transition {code}'
for code in [0,1,8]:assert values[f'2:{code}']=={-1,1},f'missing relative direction {code}'
'''
