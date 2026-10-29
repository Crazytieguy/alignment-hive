# Read inert parquet/nested Inspect zip records; never import or execute dataset code.
import io
import json
import sys
import zipfile

if sys.argv[1] == 'parquet':
    # Optional explicit installed dependency directory, never a dataset directory.
    if len(sys.argv) > 3:
        sys.path.append(sys.argv[3])
    import pyarrow.parquet as pq
    for row in pq.read_table(sys.argv[2]).to_pylist():
        print(json.dumps({k: v for k, v in row.items() if not k.startswith(('ft_', 'sw_'))}, default=str))
elif sys.argv[1] == 'collect':
    with open(sys.argv[3]) as manifest:
        wanted = {json.loads(line)['uuid'] for line in manifest}
    found = set()
    with zipfile.ZipFile(sys.argv[2]) as outer:
        names = [n for n in outer.namelist() if n.endswith('.eval') and not n.startswith('__MACOSX')]
        for name in names:
            with zipfile.ZipFile(io.BytesIO(outer.read(name))) as inner:
                for entry in inner.namelist():
                    if not entry.startswith('samples/') or not entry.endswith('.json'):
                        continue
                    s = json.loads(inner.read(entry))
                    uuid = s.get('uuid')
                    if uuid not in wanted or uuid in found:
                        continue
                    found.add(uuid)
                    # Never pass tool results, assistant text/reasoning, system prompts or scores downstream.
                    calls = []
                    for message in s.get('messages', []):
                        if message.get('role') == 'assistant':
                            calls.extend(message.get('tool_calls') or [])
                    print(json.dumps({'uuid': uuid, 'id': s.get('id'), 'input': s.get('input'),
                        'metadata': s.get('metadata'), 'calls': calls, 'file': name}, ensure_ascii=False), flush=True)
            if found == wanted:
                break
    print(f'found {len(found)}/{len(wanted)}', file=sys.stderr)
    if found != wanted:
        raise SystemExit('Missing selected UUIDs')
else:
    raise SystemExit('Expected parquet or collect')
