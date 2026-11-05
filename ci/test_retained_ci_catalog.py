#!/usr/bin/env python3
"""Actual catalog producer binding guards; no runtime/backend qualification."""
import hashlib,importlib.util,json,os,shutil,sys,tempfile,unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('catalog',Path(__file__).with_name('generate-retained-ci.py'))
catalog=importlib.util.module_from_spec(spec);spec.loader.exec_module(catalog)
class CatalogBindings(unittest.TestCase):
    def test_distinct_entries_bind_actual_elf_metadata_and_refuse_changed_or_duplicate_rows(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary).resolve();selected={}
            for name in ['ptrace','sabre','e9patch']:
                path=root/name;shutil.copyfile(Path(sys.executable).resolve(),path);path.chmod(0o700);selected[name]=path
            rows=[]
            for name,path in selected.items():
                stat=path.stat();rows.append(dict(backend=name,producer_backend=name,executable=str(path),sha256=hashlib.sha256(path.read_bytes()).hexdigest(),binary_build_sha='a'*40,dev=stat.st_dev,ino=stat.st_ino,size=stat.st_size,mtime=stat.st_mtime_ns//10**9,mtime_nsec=stat.st_mtime_ns%10**9,ctime=stat.st_ctime_ns//10**9,ctime_nsec=stat.st_ctime_ns%10**9))
            document=root/'bindings.json'
            def write(values):document.write_text(json.dumps(dict(schema=1,bindings=values)))
            write(rows);catalog.bindings(document,selected)
            for bad in [rows[:-1],rows+[rows[0]], [{**row,'sha256':'0'*64} if row['backend']=='sabre' else row for row in rows]]:
                write(bad)
                with self.assertRaises(ValueError):catalog.bindings(document,selected)
            write(rows);selected['sabre'].write_bytes(b'changed')
            with self.assertRaises(ValueError):catalog.bindings(document,selected)
    def test_pinned_inputs_use_only_the_actual_paired_mounts_and_distinct_outputs(self):
        core=Path('/host/core');project=Path('/host/project');out=Path('/out/core');other=Path('/out/project')
        self.assertEqual(catalog.mounted(out/'target/validate/helper',core,project,out,other),'/src/target/validate/helper')
        self.assertEqual(catalog.mounted(other/'target/debug/hermit-sabre',core,project,out,other),'/project/target/debug/hermit-sabre')
        self.assertEqual(catalog.mounted(core/'Cargo.toml',core,project,out,other),'/src/Cargo.toml')
        with self.assertRaisesRegex(ValueError,'outside the paired'):
            catalog.mounted(Path('/unmounted/helper'),core,project,out,other)
    def test_release_units_and_engine_profile_are_bound_to_complete_actual_cargo_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary).resolve();source=root/'core';origin=source/'inguest-engine/src/lib.rs';origin.parent.mkdir(parents=True);origin.write_text('// fixture source')
            executable=source/'target/release/hermit';executable.parent.mkdir(parents=True);shutil.copyfile(Path(sys.executable).resolve(),executable)
            profile=dict(opt_level='3',test=False,debug_assertions=False,overflow_checks=False)
            unit=dict(reason='compiler-artifact',package_id='fixture-engine',executable=str(executable),target=dict(src_path=str(origin),kind=['bin'],crate_types=['bin']),profile=profile,features=[])
            engine_unit={**unit,'executable':None,'target':dict(src_path=str(origin),kind=['cdylib'],crate_types=['cdylib'])}
            event=root/'release.jsonl'
            def write(rows):event.write_text(''.join(json.dumps(row)+'\n' for row in rows))
            write([unit,engine_unit,dict(reason='build-finished',success=True)])
            catalog.release_build_event(event,executable,source)
            record=dict(cargo_package_id='fixture-engine',profile=profile,features=[],source_records=dict(cargo_events=dict(file=event.name,bytes=event.stat().st_size,sha256=hashlib.sha256(event.read_bytes()).hexdigest())))
            catalog.release_engine_event(event,record,source)
            with self.assertRaisesRegex(ValueError,'differ from actual Cargo evidence'):
                catalog.release_engine_event(event,{**record,'profile':{**profile,'opt_level':'0'}},source)
            write([unit,engine_unit,dict(reason='build-finished',success=False)])
            with self.assertRaisesRegex(ValueError,'successful complete release'):
                catalog.release_build_event(event,executable,source)
            with self.assertRaisesRegex(ValueError,'differ from the installed producer record'):
                catalog.release_engine_event(event,record,source)
            unit['profile']={**profile,'debug_assertions':True};write([unit,engine_unit,dict(reason='build-finished',success=True)])
            with self.assertRaisesRegex(ValueError,'actual release entry profile'):
                catalog.release_build_event(event,executable,source)
    def test_release_evidence_read_accepts_owner_set_old_atime_without_custody_change(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary).resolve();source=root/'core';origin=source/'src/main.rs';origin.parent.mkdir(parents=True);origin.write_text('// fixture source')
            executable=source/'target/release/hermit';executable.parent.mkdir(parents=True);shutil.copyfile(Path(sys.executable).resolve(),executable)
            profile=dict(opt_level='3',test=False,debug_assertions=False,overflow_checks=False)
            unit=dict(reason='compiler-artifact',package_id='fixture-bin',executable=str(executable),target=dict(src_path=str(origin),kind=['bin'],crate_types=['bin']),profile=profile,features=[])
            event=root/'release.jsonl';raw=''.join(json.dumps(row)+'\n' for row in [unit,dict(reason='build-finished',success=True)]).encode();event.write_bytes(raw)
            # This is our own temporary file. Updating its atime needs no privilege.
            # Relatime may advance atime on read; noatime mounts may leave it alone.
            os.utime(event,ns=(0,event.stat().st_mtime_ns))
            before=event.stat()
            self.assertEqual(catalog.release_build_event(event,executable,source),hashlib.sha256(raw).hexdigest())
            after=event.stat()
            self.assertEqual((before.st_dev,before.st_ino,before.st_mode,before.st_size,before.st_mtime_ns,before.st_ctime_ns),(after.st_dev,after.st_ino,after.st_mode,after.st_size,after.st_mtime_ns,after.st_ctime_ns))
if __name__=='__main__':unittest.main()
