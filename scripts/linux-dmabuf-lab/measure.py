# SPDX-License-Identifier: GPL-3.0-or-later
import os,sys,json,time,subprocess,pathlib
G=pathlib.Path(os.environ['AEROFTP_GUI_TEST_ROOT'])
root=int((G/'app.pid').read_text())

def snapshot():
 table={}
 for p in pathlib.Path('/proc').iterdir():
  if not p.name.isdigit():continue
  try:
   data=(p/'stat').read_text(); fields=data[data.rindex(')')+2:].split();table[int(p.name)]=(int(fields[1]),fields)
  except (OSError,ValueError):continue
 scope={root}
 for _ in range(6):scope.update(pid for pid,(ppid,_) in table.items() if ppid in scope)
 cpu=0;rss=0;engines={};clients=set();names=[]
 for pid in scope:
  if pid not in table:continue
  _,f=table[pid];cpu+=int(f[11])+int(f[12]);rss+=int(f[21])*os.sysconf('SC_PAGE_SIZE')
  try:names.append((pid,pathlib.Path(f'/proc/{pid}/comm').read_text().strip()))
  except OSError:pass
  try:
   for fd in pathlib.Path(f'/proc/{pid}/fdinfo').iterdir():
    try:lines=fd.read_text().splitlines()
    except OSError:continue
    vals=dict(line.split(':',1) for line in lines if ':' in line)
    cid=vals.get('drm-client-id')
    client=(vals.get('drm-driver'),vals.get('drm-pdev'),cid)
    if cid is None or client in clients:continue
    clients.add(client)
    for k,v in vals.items():
     if k.startswith('drm-engine-') and not k.startswith('drm-engine-capacity-'):engines[k]=engines.get(k,0)+int(v.strip().split()[0])
  except OSError:pass
 return dict(cpu_ticks=cpu,rss_bytes=rss,engines=engines,processes=sorted(names))

name,script=sys.argv[1:3];before=snapshot();t=time.monotonic()
env=os.environ|{'AEROFTP_GUI_TEST_ROOT':str(G),'INSPECTOR_PORT':os.environ['INSPECTOR_PORT']}
cmd=['node',str(pathlib.Path(os.environ['AEROFTP_GUI_CHECKOUT'])/'scripts/gui-controller-dev.mjs'),'--js',script]
try:
 p=subprocess.run(cmd,env=env,text=True,capture_output=True,timeout=60)
except subprocess.TimeoutExpired as error:
 # TimeoutExpired may contain bytes even when text=True was requested.
 stdout=error.stdout or ''
 stderr=error.stderr or ''
 if isinstance(stdout,bytes):stdout=stdout.decode('utf-8',errors='replace')
 if isinstance(stderr,bytes):stderr=stderr.decode('utf-8',errors='replace')
 p=subprocess.CompletedProcess(cmd,124,stdout,stderr+'\nController timed out after 60 seconds')
elapsed=time.monotonic()-t;after=snapshot()
try:result=json.loads(p.stdout)
except json.JSONDecodeError:result={'stdout':p.stdout}
if not isinstance(result,dict):result={'stdout':p.stdout}
cpu_percent=(after['cpu_ticks']-before['cpu_ticks'])/os.sysconf('SC_CLK_TCK')/elapsed*100 if elapsed>0 else 0.0
result.update(exit_code=p.returncode,stderr=p.stderr,elapsed_seconds=elapsed,cpu_core_percent=cpu_percent,rss_bytes_after=after['rss_bytes'],gpu_engine_ns={k:v-before['engines'].get(k,0) for k,v in after['engines'].items()},processes=after['processes'])
(G/'evidence'/f'{name}.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps({k:v for k,v in result.items() if k!='processes'}))
sys.exit(p.returncode)
