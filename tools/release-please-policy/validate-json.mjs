import fs from 'node:fs';
import Ajv from 'ajv';
import Ajv2020 from 'ajv/dist/2020.js';
import addFormats from 'ajv-formats';

if (process.argv.length !== 4) {
  console.error('usage: validate-json.mjs <schema.json> <document.json>');
  process.exit(2);
}

const schema = JSON.parse(fs.readFileSync(process.argv[2]));
const document = JSON.parse(fs.readFileSync(process.argv[3]));
const is2020 = schema.$schema === 'https://json-schema.org/draft/2020-12/schema';
const ajv = is2020
  ? new Ajv2020({allErrors: true, strict: false})
  : new Ajv({allErrors: true, strict: false});
addFormats(ajv);
ajv.addFormat('uint64', {
  type: 'number',
  validate: value => Number.isSafeInteger(value) && value >= 0,
});
const validate = ajv.compile(schema);
if (!validate(document)) {
  console.error(ajv.errorsText(validate.errors, {separator: '\n'}));
  process.exit(1);
}
