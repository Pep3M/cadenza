import json
import os
from mcpclient import Client
CADENZA = os.environ.get('CADENZA', '../../target/release/cadenza')
c = Client(CADENZA)
C = c.call
C('open_project', path='first_light.cadenza.json')
p = C('get_project')
T = {t['name']: t for t in p['tracks']}
master_ins = {d['name'].lower(): d['id'] for d in p['master']['inserts']} if p['master']['inserts'] and 'name' in p['master']['inserts'][0] else None
print('master inserts:', p['master']['inserts'])
#
C('set_channel', track_id=T['Drums']['id'], volume_db=-6)
C('set_channel', track_id=T['Keys']['id'], volume_db=-7)
C('set_channel', track_id=T['Bass']['id'], volume_db=-1)
C('set_channel', track_id=T['Lead']['id'], volume_db=-2)
ins = {d['kind']: d['id'] for d in p['master']['inserts']}
lim = {'id': ins['builtin:limiter']}; eq = {'id': ins['builtin:eq']}
C('set_device_params', device_id=lim['id'], params={'ceiling':-2,'input_gain':4})
C('set_device_params', device_id=eq['id'], params={'low_shelf_freq':120,'low_shelf_gain':-2.5,'high_shelf_gain':1.5})
r = C('analyze_mix', target_lufs=-14)
print(json.dumps({k: r['master'][k] for k in ['lufs_integrated','true_peak_db','crest_factor_db','band_energy_db']}))
print('\n'.join(r['observations']))
C('save_project')
out = C('render', path='first_light.wav')
print('render', out['duration_seconds'], 's', out['master']['lufs_integrated'], 'LUFS')
h=C('history'); print('history:', str(h)[:200]) #, len(C('history')['undo'] if isinstance(C('history'), dict) and 'undo' in C('history') else C('history')))
