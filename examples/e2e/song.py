import json
import os
from mcpclient import Client
CADENZA = os.environ.get('CADENZA', '../../target/release/cadenza')
c = Client(CADENZA)
C = c.call
C('new_project', name='First Light', bpm=92)
for beat, name in [(0,'Intro'),(16,'Verse'),(48,'Chorus'),(80,'Outro')]:
    C('add_marker', beat=beat, name=name)

verb = C('create_track', name='Reverb', kind='bus')['track_id']
C('insert_device', track_id=verb, device='reverb', params={'size':70,'decay':2.8,'mix':100,'pre_delay':30,'low_cut':250})
dly = C('create_track', name='Delay', kind='bus')['track_id']
C('insert_device', track_id=dly, device='delay', params={'sync':0,'time':489,'feedback':38,'ping_pong':1,'mix':100,'high_cut':4500,'low_cut':300})

def track(name, device, params, color):
    return C('create_track', name=name, kind='instrument', color=color, instrument={'device':device,'params':params})['track_id']

drums = track('Drums','drum_machine',{'kick_punch':70,'snare_snappy':60},'#E85D4A')
bass = track('Bass','poly_synth',{'osc1_wave':1,'osc2_wave':2,'osc2_semitones':-12,'osc2_cents':0,'osc_mix':40,'filter_cutoff':500,'filter_resonance':20,'filter_env_amount':18,'filter_decay':250,'amp_attack':2,'amp_decay':300,'amp_sustain':65,'amp_release':80,'glide':30},'#4A7FE8')
keys = track('Keys','electric_piano',{'brightness':55,'tremolo_rate':4.5,'tremolo_depth':25},'#E8B44A')
pad = track('Pad','poly_synth',{'osc1_wave':1,'osc2_wave':1,'osc2_cents':12,'unison':3,'unison_detune':35,'filter_cutoff':1600,'filter_env_amount':8,'amp_attack':900,'amp_decay':1500,'amp_sustain':80,'amp_release':2200},'#9A6AE8')
lead = track('Lead','poly_synth',{'osc1_wave':1,'osc2_wave':2,'osc2_cents':9,'filter_cutoff':1400,'filter_env_amount':36,'filter_decay':280,'filter_sustain':10,'amp_attack':3,'amp_decay':350,'amp_sustain':25,'amp_release':220},'#4AE8A0')

# drums
C('write_drum_pattern', track_id=drums, start_beat=16, bars=8, swing=0.12, lanes={
  'kick':'x.....x...x.....', 'snare':'....X.......X..o', 'closed_hat':'x.x.x.x.x.x.x.x.'})
C('write_drum_pattern', track_id=drums, start_beat=48, bars=8, swing=0.12, lanes={
  'kick':'x.....x.x.x...x.', 'snare':'....X.......X...', 'clap':'....x.......x...',
  'closed_hat':'xoxoxoxoxoxoxo..', 'open_hat':'..............x.'})
C('write_drum_pattern', track_id=drums, start_beat=48, bars=1, lanes={'crash':'X...............'})
C('write_drum_pattern', track_id=drums, start_beat=80, bars=1, lanes={'crash':'X...............','kick':'x...............'})

verse = 'Am9 Fmaj7 Cmaj7 G6'; chorus = 'Fmaj7 G6 Em7 Am9'
C('write_chord_progression', track_id=keys, start_beat=0, progression=verse, beats_per_chord=4, voicing='drop2', pattern='sustain', velocity=0.55, center_pitch='C4')
C('write_chord_progression', track_id=keys, start_beat=16, progression=verse, beats_per_chord=4, repeat=2, voicing='drop2', pattern='arpeggio_updown', rate_beats=0.5, velocity=0.5, center_pitch='C4')
C('write_chord_progression', track_id=keys, start_beat=48, progression=chorus, beats_per_chord=4, repeat=2, voicing='drop2', pattern='pulse', rate_beats=1, velocity=0.6, center_pitch='C4')
C('write_chord_progression', track_id=keys, start_beat=80, progression='Am9 Fmaj7 Cmaj7 Am9', beats_per_chord=4, voicing='drop2', pattern='sustain', velocity=0.5, center_pitch='C4')
C('write_chord_progression', track_id=pad, start_beat=16, progression=verse, beats_per_chord=4, repeat=2, voicing='open', pattern='sustain', velocity=0.5, center_pitch='A3')
C('write_chord_progression', track_id=pad, start_beat=48, progression=chorus, beats_per_chord=4, repeat=2, voicing='open', pattern='sustain', velocity=0.6, center_pitch='A3')

def bassline(start, roots, bars_repeat):
    notes = []
    for r in range(bars_repeat):
        for i, root in enumerate(roots):
            b = (r*len(roots)+i)*4
            notes += [{'pitch':root,'start':b,'length':1.4,'velocity':0.9},
                      {'pitch':root,'start':b+1.5,'length':0.45,'velocity':0.7},
                      {'pitch':root+12,'start':b+2.5,'length':0.45,'velocity':0.75},
                      {'pitch':root,'start':b+3,'length':0.9,'velocity':0.8}]
    clip = C('add_midi_clip', track_id=bass, start_beat=start, length_beats=len(roots)*4*bars_repeat, name='Bass')['clip_id']
    C('add_notes', clip_id=clip, notes=notes)
bassline(16, [33,29,36,31], 2)   # A F C G
bassline(48, [29,31,28,33], 2)   # F G E A

mel = [('A4',0,1),('C5',1,.5),('A4',1.5,.5),('G4',2,1.5),('E4',4,.5),('G4',4.5,.5),('A4',5,1),('D5',6,1.5),
       ('E5',8,1.5),('D5',9.5,.5),('C5',10,1),('A4',11,1),('C5',12,.5),('A4',12.5,3)]
mel2 = [(p,s+16,l) for p,s,l in mel[:-2]] + [('E5',28,1),('D5',29,.5),('C5',29.5,.5),('A4',30,2)]
clip = C('add_midi_clip', track_id=lead, start_beat=48, length_beats=32, name='Hook')['clip_id']
C('add_notes', clip_id=clip, notes=[{'pitch':p,'start':s,'length':l,'velocity':0.8} for p,s,l in mel+mel2])
C('humanize_notes', clip_id=clip, timing_beats=0.015, velocity=0.08, seed=7)

for t, lvl in [(keys,-14),(pad,-12),(lead,-10),(drums,-22)]:
    C('add_send', track_id=t, bus_track_id=verb, level_db=lvl)
C('add_send', track_id=lead, bus_track_id=dly, level_db=-12)
C('add_send', track_id=keys, bus_track_id=dly, level_db=-22)

C('insert_device', track_id=bass, device='eq', params={'hp_freq':35})
C('insert_device', track_id=bass, device='compressor', params={'threshold':-20,'ratio':4,'attack':10,'release':120})
C('insert_device', track_id=pad, device='eq', params={'hp_freq':180,'high_shelf_gain':-3})
C('insert_device', track_id=keys, device='eq', params={'hp_freq':120})
C('insert_device', track_id=lead, device='chorus', params={'mix':30})
C('insert_device', track_id=drums, device='compressor', params={'threshold':-14,'ratio':3,'attack':15,'release':80,'mix':60})
C('insert_device', device='eq', params={'hp_freq':25})
C('insert_device', device='compressor', params={'threshold':-12,'ratio':2,'attack':30,'release':200,'makeup':1})
C('insert_device', device='limiter', params={'ceiling':-1})

# pad swells in over the intro via automation
C('set_channel', track_id=pad, volume_db=-8)
C('set_channel', track_id=lead, volume_db=-4, pan=0.1)
C('set_channel', track_id=keys, volume_db=-3, pan=-0.15)
C('set_channel', track_id=bass, volume_db=-2)
C('set_channel', track_id=drums, volume_db=-1)
C('save_project', path='first_light.cdz')
r = C('analyze_mix', target_lufs=-14)
print(json.dumps({'master': {k: r['report']['master'][k] if 'report' in r else None for k in []}}))
print(json.dumps(r, indent=1)[:3500])
