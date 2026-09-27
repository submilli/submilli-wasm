import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const module = await WebAssembly.compile(await readFile(process.argv[2]));
assert.deepEqual(WebAssembly.Module.imports(module), []);
const instance = await WebAssembly.instantiate(module);
assert.equal(instance.exports.main(0, 0), 0);
console.log('wasm32 interpreter smoke tests passed');
