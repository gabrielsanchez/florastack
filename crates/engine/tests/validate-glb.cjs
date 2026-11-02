// Khronos' official validator checks container, schema, accessors, geometry and embedded texture.
const fs = require('node:fs');
const validator = require(process.argv[2]);
validator.validateBytes(new Uint8Array(fs.readFileSync(process.argv[3])), { maxIssues: 100 }).then(report => {
  process.stdout.write(JSON.stringify(report.issues));
  process.exitCode = report.issues.numErrors || report.issues.numWarnings ? 1 : 0;
}).catch(error => { process.stderr.write(String(error)); process.exitCode = 1; });
