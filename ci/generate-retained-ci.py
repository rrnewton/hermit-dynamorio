#!/usr/bin/env python3
"""Generate an owned CI graph from its exact catalog and actual measured CLI inputs.
Generation is source maintenance, not execution or qualification. A graph must
be registered/tracked before the required prepared-project consumer admits it.
"""
import argparse,copy,hashlib,json,os,re,stat
from pathlib import Path

def absolute(value):
    path=Path(value)
    if not path.is_absolute() or path.is_symlink(): raise ValueError('explicit canonical nonsymlink input required')
    result=path.resolve(strict=True)
    if result!=path: raise ValueError('noncanonical input')
    return path

def document(path):
    data=path.read_bytes()
    if len(data)>16*1024*1024: raise ValueError('catalog input exceeds bound')
    def unique(pairs):
        result={}
        for key,value in pairs:
            if key in result:raise ValueError('duplicate JSON object key: '+key)
            result[key]=value
        return result
    return json.loads(data,object_pairs_hook=unique)

def entries(values):
    result={}
    for value in values:
        name,separator,path=value.partition('=')
        if not separator or not re.fullmatch('[a-z][a-z0-9-]*',name) or name in result: raise ValueError('malformed/duplicate named entry')
        result[name]=absolute(path)
    return result

def bindings(path,selected,guest_paths=None):
    obj=document(path)
    if set(obj)!={'schema','bindings'} or obj['schema']!=1 or not obj['bindings']: raise ValueError('closed nonempty binding schema1 required')
    keys={'backend','producer_backend','executable','sha256','binary_build_sha','dev','ino','size','mtime','mtime_nsec','ctime','ctime_nsec'}
    seen=set()
    for row in obj['bindings']:
        if set(row)!=keys or row['backend'] in seen or row['backend'] not in selected: raise ValueError('unknown/duplicate binding')
        seen.add(row['backend']);path=selected[row['backend']]
        expected=guest_paths[row['backend']] if guest_paths is not None else str(path)
        if row['executable']!=expected: raise ValueError('named entry differs from actual binding')
        before=path.stat()
        if not stat.S_ISREG(before.st_mode) or not before.st_mode&0o111 or before.st_size==0: raise ValueError('entry is not executable regular data')
        expected={'dev':before.st_dev,'ino':before.st_ino,'size':before.st_size,'mtime':before.st_mtime_ns//10**9,'mtime_nsec':before.st_mtime_ns%10**9,'ctime':before.st_ctime_ns//10**9,'ctime_nsec':before.st_ctime_ns%10**9}
        if any(row[k]!=v for k,v in expected.items()): raise ValueError('selected executable metadata changed')
        with path.open('rb') as source:
            if source.read(4)!=b'\x7fELF': raise ValueError('selected entry is not actual ELF')
            source.seek(0);digest=hashlib.file_digest(source,'sha256').hexdigest()
        if digest!=row['sha256'] or path.stat()!=before: raise ValueError('selected executable changed while binding')
        if not re.fullmatch('[0-9a-fA-F]{7,40}',row['binary_build_sha']) or not row['producer_backend']: raise ValueError('missing actual producer identity')
    if seen!=set(selected): raise ValueError('named entries and actual binding population differ')
    # Static extension frontends cannot be the original core comparator.
    core=selected['ptrace']
    if any(path.samefile(core) for name,path in selected.items() if name!='ptrace'): raise ValueError('self parity is not a static extension')

def stable_metadata(value):
    return tuple(getattr(value,name) for name in ('st_dev','st_ino','st_mode','st_size','st_mtime_ns','st_ctime_ns'))

def evidence_bytes(path):
    limit=16*1024*1024;before=path.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_size>limit: raise ValueError('Cargo evidence must be nonsymlink regular data at most16MiB')
    flags=os.O_RDONLY|os.O_CLOEXEC|os.O_NOFOLLOW|os.O_NONBLOCK
    with os.fdopen(os.open(path,flags),'rb') as source:
        opened=os.fstat(source.fileno())
        if stable_metadata(opened)!=stable_metadata(before): raise ValueError('Cargo evidence identity changed before read')
        raw=source.read(limit+1)
        after=os.fstat(source.fileno())
    if len(raw)>limit or len(raw)!=before.st_size or stable_metadata(after)!=stable_metadata(before) or stable_metadata(path.lstat())!=stable_metadata(before): raise ValueError('Cargo evidence identity/length changed during read')
    return raw

def release_build_event(path, executable, source):
    raw=evidence_bytes(path)
    records=[json.loads(line) for line in raw.splitlines() if line.strip()]
    if not records or records[-1].get('reason')!='build-finished' or records[-1].get('success') is not True: raise ValueError('successful complete release Cargo events required')
    units=[r for r in records if r.get('reason')=='compiler-artifact' and r.get('executable')==str(executable)]
    if len(units)!=1: raise ValueError('exactly one actual release entry Cargo unit required')
    unit=units[0];profile=unit.get('profile',{});target=unit.get('target',{})
    if profile.get('opt_level')!='3' or profile.get('test') is not False or profile.get('debug_assertions') is not False or profile.get('overflow_checks') is not False: raise ValueError('actual release entry profile required; no validate/dev fallback')
    origin=absolute(target.get('src_path',''))
    if source not in origin.parents or 'bin' not in target.get('kind',[]): raise ValueError('release entry unit is not the actual source-owned binary')
    return hashlib.sha256(raw).hexdigest()


def release_engine_event(path, record, source):
    raw=evidence_bytes(path)
    expected=record['source_records']['cargo_events']
    if expected!={'file':path.name,'bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest()}: raise ValueError('release engine events differ from the installed producer record')
    records=[json.loads(line) for line in raw.splitlines() if line.strip()]
    if not records or records[-1].get('reason')!='build-finished' or records[-1].get('success') is not True: raise ValueError('successful complete release engine Cargo events required')
    units=[row for row in records if row.get('reason')=='compiler-artifact' and row.get('package_id')==record['cargo_package_id'] and 'cdylib' in row.get('target',{}).get('crate_types',[])]
    if len(units)!=1: raise ValueError('exactly one actual shared engine Cargo unit required')
    unit=units[0];profile=unit.get('profile',{})
    if profile!=record['profile'] or unit.get('features')!=record['features']: raise ValueError('installed engine profile/features differ from actual Cargo evidence')
    if profile.get('opt_level')!='3' or profile.get('test') is not False or profile.get('debug_assertions') is not False or profile.get('overflow_checks') is not False: raise ValueError('release compatibility requires the actual normal release engine profile')
    origin=absolute(unit.get('target',{}).get('src_path',''))
    if source/'inguest-engine' not in origin.parents: raise ValueError('release engine unit is not the actual core source owner')
    return hashlib.sha256(raw).hexdigest()


def emit(path,value):
    path=Path(path)
    if not path.is_absolute(): raise ValueError('output must be explicit absolute path')
    with path.open('x') as output: json.dump(value,output,indent=2);output.write('\n');output.flush();os.fsync(output.fileno())

def mounted(path,core,project,core_out,project_out):
    for host,guest in ((core_out/'target','/src/target'),(project_out/'target','/project/target'),(core,'/src'),(project,'/project')):
        if path==host: return guest
        if host in path.parents: return guest+'/'+str(path.relative_to(host))
    raise ValueError('pinned input is outside the paired source/output mounts: '+str(path))

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('core-dag','catalog','bindings','pinned-bindings','output','core-source','extension-root','core-tools','pinned-core-tools','host-descriptor','pinned-descriptor','core-pinned-out','project-pinned-out','inputs'): p.add_argument('--'+name,required=True)
    p.add_argument('--entry',action='append',required=True,help='logical backend=actual canonical host ELF')
    p.add_argument('--pinned-entry',action='append',required=True,help='logical backend=actual canonical pinned-output ELF')
    p.add_argument('--release-entry',action='append',default=[])
    p.add_argument('--release-bindings')
    p.add_argument('--release-cargo-event',action='append',default=[])
    p.add_argument('--release-engine-cargo-events')
    a=p.parse_args(argv);core=absolute(a.core_source);project=absolute(a.extension_root);tools=absolute(a.core_tools)
    core_out=absolute(a.core_pinned_out);project_out=absolute(a.project_pinned_out);pinned_tools=absolute(a.pinned_core_tools)
    # Host and pinned prepared executables have distinct actual output owners.
    # The prepared-project reader still holds/revalidates the original core record.
    if core/'target' not in tools.parents or core_out/'target' not in pinned_tools.parents or tools.samefile(pinned_tools): raise ValueError('distinct actual host and pinned core tool directories required')
    if core==project or core_out==project_out: raise ValueError('distinct source/output roots required')
    graph=document(absolute(a.core_dag));catalog=document(absolute(a.catalog));selected=entries(a.entry);pinned=entries(a.pinned_entry)
    bound=absolute(a.bindings);pinned_bound=absolute(a.pinned_bindings)
    if catalog['schema']!=1 or catalog['owner'] not in ('patching','dynamorio'): raise ValueError('unknown catalog owner')
    owner=catalog['owner'];required={'ptrace','sabre','e9patch'} if owner=='patching' else {'ptrace','dbt'}
    if set(selected)!=required or set(pinned)!=required: raise ValueError('exact owner entry population required in both contexts')
    guest_paths={name:mounted(path,core,project,core_out,project_out) for name,path in pinned.items()}
    bindings(bound,selected);bindings(pinned_bound,pinned,guest_paths)
    descriptors={'host':absolute(a.host_descriptor),'pinned':absolute(a.pinned_descriptor)}
    for context,path in descriptors.items():
        obj=document(path);expected={'schema':1,'repository':str(project) if context=='host' else '/project','graph':str(Path(a.output)) if context=='host' else '/project/ci/dag/validate.json','tools_root':str(core) if context=='host' else '/src','shared_package':'hermit-run'}
        if obj!=expected: raise ValueError('project descriptor is not the exact source/graph/tool cohort')
    inputs=document(absolute(a.inputs))
    needs_release=any(row.get('prepared_context')=='release' for row in catalog['steps'])
    input_contexts=('host','pinned','release') if needs_release else ('host','pinned')
    if set(inputs)!={'schema',*input_contexts} or inputs['schema']!=1: raise ValueError('closed context input map required')
    required_inputs={'HERMIT_PATCHING_COHORT','HERMIT_SABRE_LOADER','HERMIT_E9TOOL','HERMIT_E9PATCH_BACKEND'} if owner=='patching' else {'HERMIT_DYNAMORIO_ENGINE_ARTIFACT','HERMIT_DYNAMORIO_ENGINE_CHECKER'}
    if any(not required_inputs <= set(inputs[context]) for context in input_contexts): raise ValueError('required actual owner cohort/checker inputs are absent')
    contexts={}
    for context in ('host','pinned'):
        values={}
        for name,value in inputs[context].items():
            if not re.fullmatch('[A-Z][A-Z0-9_]*',name): raise ValueError('invalid explicit input name')
            actual=absolute(value);values[name]=str(actual) if context=='host' else mounted(actual,core,project,core_out,project_out)
        values.update(HERMIT_CORE_SOURCE=str(core) if context=='host' else '/src',HERMIT_EXTENSION_ROOT=str(project) if context=='host' else '/project',HERMIT_CORE_TOOLS=str(tools) if context=='host' else mounted(pinned_tools,core,project,core_out,project_out),HERMIT_CORE_BIN=str(selected['ptrace']) if context=='host' else guest_paths['ptrace'],HERMIT_E2E_COMMAND_BINDINGS=str(bound) if context=='host' else mounted(pinned_bound,core,project,core_out,project_out),HERMIT_NEXTEST_PROJECT=str(descriptors['host']) if context=='host' else '/project/ci/prepared-project-pinned.json')
        for name in required:
            values['HERMIT_'+name.upper()+'_BIN']=str(selected[name]) if context=='host' else guest_paths[name]
            values['HERMIT_'+name.upper()+'_BINDINGS']=values['HERMIT_E2E_COMMAND_BINDINGS']
        # Historical host tests use these actual package-specific input names.
        values['HERMIT_PATCHING_ROOT' if owner=='patching' else 'HERMIT_DYNAMORIO_ROOT']=values['HERMIT_EXTENSION_ROOT']
        if owner=='dynamorio':values['HERMIT_DYNAMORIO_BIN']=values['HERMIT_DBT_BIN']
        contexts[context]=values
    if needs_release:
        if not a.release_bindings: raise ValueError('explicit release command binding required')
        release=entries(a.release_entry);events=entries(a.release_cargo_event)
        if set(release)!=required or set(events)!=required: raise ValueError('exact release entry/event population required')
        release_bound=absolute(a.release_bindings);bindings(release_bound,release)
        release_build_event(events['ptrace'],release['ptrace'],core)
        for name in required-{'ptrace'}: release_build_event(events[name],release[name],project)
        values={}
        for name,value in inputs['release'].items():
            if not re.fullmatch('[A-Z][A-Z0-9_]*',name): raise ValueError('invalid explicit release input name')
            values[name]=str(absolute(value))
        engine=document(absolute(values['HERMIT_DYNAMORIO_ENGINE_ARTIFACT'])) if owner=='dynamorio' else None
        if engine is not None:
            if not a.release_engine_cargo_events: raise ValueError('actual release engine Cargo evidence is required')
            release_engine_event(absolute(a.release_engine_cargo_events),engine,core)
        values.update(HERMIT_CORE_SOURCE=str(core),HERMIT_EXTENSION_ROOT=str(project),HERMIT_CORE_TOOLS=str(tools),HERMIT_CORE_BIN=str(release['ptrace']),HERMIT_BIN=str(release['ptrace']),HERMIT_E2E_COMMAND_BINDINGS=str(release_bound))
        for name in required:
            values['HERMIT_'+name.upper()+'_BIN']=str(release[name]);values['HERMIT_'+name.upper()+'_BINDINGS']=str(release_bound)
        values['HERMIT_PATCHING_ROOT' if owner=='patching' else 'HERMIT_DYNAMORIO_ROOT']=str(project)
        if owner=='dynamorio': values['HERMIT_DYNAMORIO_BIN']=values['HERMIT_DBT_BIN']
        contexts['release']=values
    elif a.release_entry or a.release_bindings or a.release_cargo_event or a.release_engine_cargo_events: raise ValueError('unused release command authority')
    steps=graph['steps'];known={r['group']+'.'+r['job'] for r in steps}
    if len(known)!=len(steps):raise ValueError('duplicate actual core DAG tag')
    import shlex
    for original in catalog['steps']:
        row=copy.deepcopy(original);tag=row['group']+'.'+row['job']
        if tag in known: raise ValueError('external node still present in selected core graph: '+tag)
        known.add(tag);env=row.setdefault('env',{});context=row.pop('prepared_context',None)
        if context is not None:
            if context not in contexts: raise ValueError('unknown prepared context')
            values=contexts[context];env.update(values);env['HERMIT_PREPARED_NEXTEST_REQUIRED']='1'
            if context=='pinned':
                # The scheduler starts this command at the core root. Arguments
                # before the inner shell are host mount inputs; actual per-context
                # artifact values are exported only inside the guest payload.
                env.update(HERMIT_CORE_SOURCE=str(core),HERMIT_EXTENSION_ROOT=str(project),HERMIT_EXTENSION_PINNED_OUT=str(project_out))
                args=shlex.split(row['cmd']);payload=args[-1]
                args[-1]='export '+ ' '.join(k+'='+shlex.quote(v) for k,v in values.items())+'; '+payload
                row['cmd']=' '.join(('"'+v+'"' if v.startswith('${') else shlex.quote(v)) for v in args)
        else:env.update(contexts['host'])
        if 'manifest' in row and ' --execution-owner '+owner+' ' not in row['cmd']: raise ValueError('owned manifest node lacks explicit execution owner: '+tag)
        steps.append(row)
    # Normal per-project preparation consumes the original core preparation and
    # builds/lists the owner selectors through the same core wrapper at its own cwd.
    all_labels=sorted({label for row in catalog['steps'] if 'NEXTEST_PREPARED_BUILD_SELECTION' in row.get('env',{}) for label in row['labels']})
    by_tag={r['group']+'.'+r['job']:r for r in steps}
    for context,anchor in (('host','build.workspace'),('pinned','build.workspace_in_pinned_root')):
        if anchor not in by_tag:raise ValueError('missing original core preparation owner '+anchor)
        row=copy.deepcopy(by_tag[anchor]);row['group']='build';row['job']=owner+'_nextest'+('_in_pinned_root' if context=='pinned' else '')
        row['fail_fast_family']=owner+'_nextest'+('_in_pinned_root' if context=='pinned' else '');row['labels']=all_labels;row['result_manifests']=[];row['deps']=[anchor,'build.rust_scripts'+('_in_pinned_root' if context=='pinned' else '')]
        row['desc']='Prepare actual '+owner+' Nextest executables with the original core tool cohort'
        row['description']='Separate project cwd and target; required original core preparation/CPU wrapper held and rechecked. Original preparation resource limits retained, execution remains unqualified until measured.'
        row['env']={**row.get('env',{}),**contexts[context],'HERMIT_PREPARED_NEXTEST_REQUIRED':'1'}
        row['env'].pop('NEXTEST_PREPARED_BUILD_SELECTION',None);row['env'].pop('NEXTEST_EXPECTED_EXECUTED',None)
        payload='export '+ ' '.join(k+'='+shlex.quote(v) for k,v in contexts[context].items())+'; cd -- '+shlex.quote(str(project) if context=='host' else '/project')+' && "${HERMIT_CORE_SOURCE:?}/ci/run-with-core-build.sh" "${HERMIT_CORE_SOURCE:?}/ci/nextest-binaries.rs" prepare full'
        if context=='host': row['cmd']=payload
        else:
            args=[str(core/'ci/hermetic/run-in-pinned-root.sh'),'--src',str(core),'--out',str(core_out),'--src-rw','--cargo-home',str(core_out/'cargo'),'--project-src',str(project),'--project-out',str(project_out),'--project-descriptor',str(descriptors['pinned']),'--project-src-rw']
            for name in sorted(row['env']):args+=['--env',name]
            row['cmd']=' '.join(map(shlex.quote,args+['--','bash','-c','/src/ci/hermetic/assert-no-network.sh && /src/ci/hermetic/assert-build-dependencies.sh && '+payload]))
        tag=row['group']+'.'+row['job']
        if tag in known:raise ValueError('duplicate project producer '+tag)
        known.add(tag);steps.append(row)
    for row in steps:
        missing=set(row['deps'])-known
        if missing: raise ValueError('unprovided dependency owner: '+repr(sorted(missing)))
    emit(a.output,graph)
if __name__=='__main__': main()
