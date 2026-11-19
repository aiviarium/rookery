// The API reference: rookeryd's by operation, from the committed operation
// documents in docs/openapi (`rookeryd operations`), and the server's by method,
// from api/methods.json (the method report of rookery-server). One folder per
// program under content/docs/api, one page per group. The overview and the
// portal page beside them are written by hand.
// Plain Node, no packages.
import { copyFileSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const docs = join(dirname(fileURLToPath(import.meta.url)), '..');

const programs = [
  {
    dir: 'host',
    file: 'rookeryd.operations.json',
    title: 'rookeryd',
    description: 'The operations of rookeryd, on every host.',
  },
];

const groups = {
  general: ['General', 'The version, and reading the configuration again.'],
  sessions: ['Sessions', 'Commands and terminals on the host, their output and input.'],
  applied: ['Applied', 'The profile the host keeps of itself, as the last apply wrote it.'],
  system: ['System', 'Load, memory and disks, and power.'],
  desktop: ['Desktop', 'The shared desktop of the agent.'],
  files: ['Files', 'Files under the configured roots, as the agent.'],
  units: ['Units', 'systemd units of the host.'],
  logs: ['Logs', "The host's journal."],
  processes: ['Processes', 'The processes of the host.'],
  channel: ['Channel', 'Messages between programs on the host and rookery.'],
  services: ['Services', 'Programs of plugins that answer calls.'],
  ports: ['Ports', "TCP connections to ports on the host's loopback."],
  endpoints: ['Endpoints', "TCP connections to addresses on the host's own networks, which only rookery serve opens, for a resource's endpoint."],
};
// A group of the same name means something else in each program.
const hostGroups = {
  actions: ['Actions', "The host's actions and their arguments."],
};
const groupOrder = [...Object.keys(hostGroups), ...Object.keys(groups)];

// ---------------------------------------------------------------------------
// Text

/** Markdown-safe text for MDX: braces and angle brackets outside code spans. */
const md = (text) =>
  String(text ?? '')
    .split(/(`[^`]*`)/)
    .map((part, index) => (index % 2 ? part : part.replace(/[{}<>]/g, (ch) => `\\${ch}`)))
    .join('');
const cell = (text) => md(text).replace(/\|/g, '\\|').replace(/\s*\n\s*/g, ' ');
const yaml = (text) => JSON.stringify(String(text));
const json = (value) => JSON.stringify(value, null, 2);
/** The anchor fumadocs gives a heading. */
const anchor = (text) =>
  text
    .toLowerCase()
    .replace(/[^a-z0-9 _-]/g, '')
    .replace(/ /g, '-');
const typeAnchor = (name) => `type-${name.toLowerCase()}`;

// ---------------------------------------------------------------------------
// Schemas

function makeSchemas(root) {
  const all = root.components?.schemas ?? {};
  const refName = (ref) => ref.replace(/^#\/components\/schemas\//, '');
  const resolve = (schema) => {
    let node = schema;
    for (let i = 0; node?.$ref && i < 20; i++) node = all[refName(node.$ref)];
    return node ?? {};
  };

  /** A short type, linking named types to the page's Types section. */
  function type(schema, used) {
    if (!schema || Object.keys(schema).length === 0) return 'any';
    if (schema.$ref) {
      const name = refName(schema.$ref);
      used.add(name);
      return `[${name}](#${typeAnchor(name)})`;
    }
    if (schema.enum) return schema.enum.map((value) => `\`${JSON.stringify(value)}\``).join(' \\| ');
    if (schema.const !== undefined) return `\`${JSON.stringify(schema.const)}\``;
    for (const key of ['oneOf', 'anyOf']) {
      if (schema[key]) return schema[key].map((s) => type(s, used)).join(' \\| ');
    }
    if (schema.allOf) return schema.allOf.map((s) => type(s, used)).join(' & ');
    const types = Array.isArray(schema.type) ? schema.type : [schema.type ?? (schema.properties ? 'object' : 'any')];
    return types
      .map((t) => {
        if (t === 'array') return `${type(schema.items, used)}[]`;
        if (t === 'object' && schema.additionalProperties && typeof schema.additionalProperties === 'object')
          return `map of ${type(schema.additionalProperties, used)}`;
        if (t === 'integer' && schema.format) return `integer`;
        return t;
      })
      .join(' \\| ');
  }

  /** A table of an object's fields, or one line for anything else. */
  function table(schema, used) {
    const node = resolve(schema);
    if (node.properties) {
      const required = new Set(node.required ?? []);
      let text = '| Field | Type | Description |\n| --- | --- | --- |\n';
      for (const [name, field] of Object.entries(node.properties)) {
        const mark = required.has(name) ? '' : ' (optional)';
        text += `| \`${name}\`${mark} | ${type(field, used)} | ${cell(field.description ?? resolve(field).description ?? '')} |\n`;
      }
      return text;
    }
    return `${type(schema, used)}\n${node.description ? `\n${md(node.description)}\n` : ''}`;
  }

  /** A plausible value, for the examples. */
  function example(schema, name = '', depth = 0) {
    const node = resolve(schema);
    // Any JSON, such as a value of the key-value store.
    if (Object.keys(node).length === 0) return { columns: 2 };
    if (node.example !== undefined) return node.example;
    if (node.examples?.length) return node.examples[0];
    if (node.enum) return node.enum[0];
    if (node.const !== undefined) return node.const;
    if (node.oneOf || node.anyOf) return example((node.oneOf ?? node.anyOf)[0], name, depth);
    if (node.allOf) return Object.assign({}, ...node.allOf.map((s) => example(s, name, depth)));
    const t = Array.isArray(node.type) ? node.type.find((x) => x !== 'null') : node.type ?? (node.properties ? 'object' : undefined);
    if (t === 'object') {
      if (depth > 3) return {};
      const out = {};
      const props = Object.entries(node.properties ?? {});
      const required = new Set(node.required ?? []);
      const shown = props.length <= 8 ? props : props.filter(([key]) => required.has(key));
      for (const [key, field] of shown) out[key] = example(field, key, depth + 1);
      return out;
    }
    if (t === 'array') return depth > 3 ? [] : [example(node.items, name, depth + 1)];
    if (t === 'integer' || t === 'number') return placeholderNumber(name);
    if (t === 'boolean') return true;
    if (t === 'string') return placeholder(name, node.format);
    return null;
  }

  /** Every named type a set of types needs, in order of first use. */
  function closure(names) {
    const seen = new Set();
    const queue = [...names];
    while (queue.length) {
      const name = queue.shift();
      if (seen.has(name) || !all[name]) continue;
      seen.add(name);
      const found = new Set();
      table({ $ref: `#/components/schemas/${name}` }, found);
      queue.push(...found);
    }
    return [...seen];
  }

  return { all, resolve, type, table, example, closure };
}

const placeholders = {
  id: 'k3v9qz2m8w1x',
  session: 'k3v9qz2m8w1x',
  name: 'status',
  host: 'studio',
  topic: 'myplugin/progress',
  namespace: 'ui',
  key: 'layout',
  unit: 'rookeryd.service',
  plugin: 'ask',
  method: 'ask',
  argument: 'snapshot',
  wait: '30s',
  path: '/srv/agent/workspaces/notes.txt',
  command: 'uname -a',
  title: 'uname -a',
  user: 'agent',
  kind: 'process',
  signal: 'TERM',
  cursor: 's=4f2a…',
  text: 'hello',
  url: 'https://example.com',
  address: '100.64.0.12',
  // The version the documents were written by.
  version: JSON.parse(readFileSync(join(docs, 'openapi', 'rookeryd.operations.json'), 'utf8')).version,
  started: '2026-11-12T09:30:00Z',
  ended: '2026-11-12T09:30:02Z',
  time: '2026-11-12T09:30:00Z',
};
function placeholder(name, format) {
  if (format === 'date-time') return '2026-11-12T09:30:00Z';
  if (name.endsWith('_b64')) return 'aGVsbG8K';
  return placeholders[name] ?? 'text';
}
function placeholderNumber(name) {
  const numbers = { pid: 4812, since: 0, limit: 100, cols: 120, rows: 40, exit_code: 0, revision: 3, api: 1, uid: 1001 };
  return numbers[name] ?? 0;
}

/** A path argument's value, from its name and where it is used. */
function argValue(op, arg) {
  if (arg.name === 'verb') {
    return op.name === 'system.power' ? 'reboot' : 'restart';
  }
  if (arg.name === 'name' && op.group === 'hosts') return 'studio';
  if (arg.name === 'name' && op.group === 'services') return 'desk';
  if (arg.name === 'id' && op.group === 'inbox') return 42;
  if (arg.name === 'id' && op.group === 'jobs') return 'j7';
  if (arg.name === 'host') return 'studio';
  if (arg.name === 'pid') return 4812;
  if (arg.name === 'topic' && op.name === 'channel.listen') return 'myplugin/';
  return placeholders[arg.name] ?? (arg.in === 'header' ? '3' : 'VALUE');
}

// ---------------------------------------------------------------------------
// Pages

function operationSection(op, program, schemas, used) {
  const stream = op.kind === 'stream';
  let text = `## \`${op.name}\`\n\n${md(op.summary)}\n\n`;

  const facts = [];
  if (op.level) facts.push(md(op.level));
  if (stream) {
    facts.push('A stream, as a subscription on the portal or a WebSocket of its own.');
    facts.push(op.resumable ? 'It resumes from `since`.' : 'It does not resume.');
  }
  if (op.portal === false) facts.push(`Not on the portal${op.http_only ? `, because ${md(op.http_only)}` : ''}.`);
  if (op.key) facts.push(`It takes ${md(op.key)} as the bearer, not the token or a role.`);
  if (facts.length) text += facts.join(' ') + '\n\n';

  if (op.args?.length) {
    text += '**Arguments**\n\n| Name | Where | Required | Description |\n| --- | --- | --- | --- |\n';
    for (const arg of op.args) {
      const where = arg.in === 'header' ? `header \`${arg.header}\`` : arg.in;
      text += `| \`${arg.name}\` | ${where} | ${arg.required ? 'yes' : 'no'} | ${cell(arg.description ?? '')} |\n`;
    }
    text += '\n';
  }
  if (op.data) {
    text += '**Input**\n\n';
    if (op.data.bytes) text += 'Bytes: `{"base64": …}` on the portal, the request body over HTTP.\n\n';
    else if (op.data.schema) {
      const free = Object.keys(schemas.resolve(op.data.schema)).length === 0;
      if (op.data.description) text += `${md(op.data.description)}\n\n`;
      if (!free) text += schemas.table(op.data.schema, used) + '\n';
      else if (!op.data.description) text += 'Any JSON value.\n\n';
    }
  }
  if (op.result) {
    text += '**Result**\n\n';
    if (op.result.bytes)
      text += `Bytes: \`{"base64": …, "content_type": …}\` on the portal, the response body over HTTP${op.result.content_type ? ` (\`${op.result.content_type}\`)` : ''}.\n\n`;
    else if (op.result.schema) text += schemas.table(op.result.schema, used) + '\n';
  }
  if (op.errors?.length) {
    text += '**Errors**\n\n| Status | When |\n| --- | --- |\n';
    for (const error of op.errors) text += `| ${error.status} | ${cell(error.description)} |\n`;
    text += '\n';
  }
  if (stream) {
    for (const [key, heading, intro] of [
      ['receives', 'Frames from the server', ''],
      ['sends', 'Frames to the server', ''],
    ]) {
      const frames = op[key] ?? [];
      if (!frames.length) continue;
      text += `**${heading}**\n\n| Frame | Kind | Description |\n| --- | --- | --- |\n`;
      for (const frame of frames) {
        const example = frame.example != null ? ` Example: \`${JSON.stringify(frame.example)}\`` : '';
        text += `| \`${frame.name}\` | ${frame.binary ? 'binary' : 'JSON'} | ${cell(frame.description ?? '')}${cell(example)} |\n`;
      }
      text += '\n' + intro;
    }
  }

  text += invocation(op, program, schemas);
  return text;
}

/** The arguments as the portal takes them, and as HTTP puts them. */
function filledArgs(op) {
  const args = {};
  for (const arg of op.args ?? []) {
    if (arg.required || arg.in === 'path' || ['wait', 'since'].includes(arg.name)) args[arg.name] = argValue(op, arg);
  }
  return args;
}

function httpTarget(op, args) {
  let path = op.http.path.replace('{*', '{');
  const query = [];
  const headers = [];
  for (const arg of op.args ?? []) {
    if (!(arg.name in args)) continue;
    const value = args[arg.name];
    if (arg.in === 'path') path = path.replace(`{${arg.name}}`, encodeURI(String(value)));
    else if (arg.in === 'query') query.push(`${arg.name}=${encodeURIComponent(String(value))}`);
    else if (arg.in === 'header') headers.push(`${arg.header}: ${value}`);
  }
  return { path: path + (query.length ? `?${query.join('&')}` : ''), headers };
}

function invocation(op, program, schemas) {
  const stream = op.kind === 'stream';
  const args = filledArgs(op);
  const data = op.data?.bytes ? { base64: 'aGVsbG8K' } : op.data?.schema ? schemas.example(op.data.schema) : undefined;
  const request = { id: 1, op: op.name };
  if (Object.keys(args).length) request.args = args;
  if (data !== undefined) request.data = data;

  let reply;
  if (stream) reply = [{ id: 1, result: { open: true } }];
  else if (op.result?.bytes) reply = [{ id: 1, result: { base64: 'aGVsbG8K', content_type: op.result.content_type ?? 'application/octet-stream' } }];
  else if (op.result?.schema) reply = [{ id: 1, result: schemas.example(op.result.schema) }];
  else reply = [{ id: 1, result: null }];
  if (stream) {
    const first = (op.receives ?? []).find((frame) => !frame.binary && frame.example != null);
    if (first) reply.push({ sub: 1, data: first.example });
    if ((op.receives ?? []).some((frame) => frame.binary)) reply.push('binary: 00 00 00 01, then the bytes');
  }
  const replyText = reply.map((frame) => (typeof frame === 'string' ? `// ${frame}` : json(frame))).join('\n\n');

  const { path, headers } = httpTarget(op, args);
  const tabs = ['Portal', 'HTTP'];

  let text = `<Tabs items={${JSON.stringify(tabs)}}>\n<Tab value="Portal">\n\n`;
  if (op.portal === false) {
    text += `Not on the portal${op.http_only ? `: ${md(op.http_only)}` : ''}.\n\n`;
  } else {
    text += `\`\`\`json title="Request"\n${json(request)}\n\`\`\`\n\n\`\`\`js title="Reply"\n${replyText}\n\`\`\`\n\n`;
    if (stream) text += 'End the subscription with `{"sub": 1, "end": true}`.\n\n';
  }
  text += '</Tab>\n<Tab value="HTTP">\n\n';
  text += httpExample(op, program, path, headers, data, stream);
  text += '</Tab>\n';
  text += '</Tabs>\n\n';
  return text;
}

function httpExample(op, program, path, headers, data, stream) {
  if (stream) {
    const base = 'ws+unix:/run/rookery/control.sock';
    return `A WebSocket of its own: \`GET ${path}\` with the upgrade headers, at \`${base}${path}\`. Its frames are the ones above, without the portal's subscription number.\n\n`;
  }
  const lines = [];
  if (op.key) lines.push(`curl -H "Authorization: Bearer $SETUP_KEY"`);
  else lines.push('sudo curl --unix-socket /run/rookery/control.sock');
  if (op.http.method !== 'GET') lines.push(`-X ${op.http.method}`);
  for (const header of headers) lines.push(`-H '${header}'`);
  if (op.data?.bytes) lines.push('--data-binary @notes.txt');
  else if (data !== undefined) lines.push(`-H 'Content-Type: application/json' -d '${JSON.stringify(data).replace(/'/g, "'\\''")}'`);
  if (op.result?.bytes) lines.push('-o result.bin');
  const host = 'http://rookeryd';
  lines.push(`'${host}${path}'`);
  return `\`${op.http.method} ${path}\`\n\n\`\`\`bash\n${lines.join(' \\\n  ')}\n\`\`\`\n\n`;
}

function typesSection(names, schemas) {
  if (!names.length) return '';
  let text = '## Types\n\nThe named types used on this page.\n\n';
  for (const name of schemas.closure(names)) {
    const node = schemas.all[name];
    text += `### \`${name}\` [#${typeAnchor(name)}]\n\n`;
    if (node.description) text += `${md(node.description)}\n\n`;
    text += schemas.table({ $ref: `#/components/schemas/${name}` }, new Set()) + '\n';
  }
  return text;
}

function groupPage(group, ops, program, schemas) {
  const [title, description] = hostGroups[group] ?? groups[group] ?? [group, ''];
  const used = new Set();
  let body = '';
  for (const op of ops) body += operationSection(op, program, schemas, used);
  let head = `---\ntitle: ${yaml(title)}\ndescription: ${yaml(description)}\n---\n\n`;
  head += '| Operation | Kind | HTTP |\n| --- | --- | --- |\n';
  for (const op of ops) head += `| [\`${op.name}\`](#${anchor(op.name)}) | ${op.kind} | \`${op.http.method} ${op.http.path.replace('{*', '{')}\` |\n`;
  return `${head}\n${body}${typesSection([...used], schemas)}`;
}

function indexPage(program, root, byGroup) {
  let text = `---\ntitle: ${yaml(program.title)}\ndescription: ${yaml(program.description)}\n---\n\n`;
  text += `${md(root.description)}\n\n`;
  text += '| Operation | Kind | Summary |\n| --- | --- | --- |\n';
  for (const group of byGroup.keys()) {
    for (const op of byGroup.get(group)) {
      text += `| [\`${op.name}\`](/docs/api/${program.dir}/${group}#${anchor(op.name)}) | ${op.kind} | ${cell(op.summary)} |\n`;
    }
  }
  return text + '\n';
}

// ---------------------------------------------------------------------------

mkdirSync(join(docs, 'public/openapi'), { recursive: true });
for (const file of readdirSync(join(docs, 'openapi'))) {
  if (file.endsWith('.json')) copyFileSync(join(docs, 'openapi', file), join(docs, 'public/openapi', file));
}

for (const program of programs) {
  const root = JSON.parse(readFileSync(join(docs, 'openapi', program.file), 'utf8'));
  const schemas = makeSchemas(root);
  const out = join(docs, 'content/docs/api', program.dir);
  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });

  const byGroup = new Map();
  for (const op of root.operations) {
    if (!byGroup.has(op.group)) byGroup.set(op.group, []);
    byGroup.get(op.group).push(op);
  }
  const order = [...byGroup.keys()].sort((a, b) => groupOrder.indexOf(a) - groupOrder.indexOf(b));
  const sorted = new Map(order.map((group) => [group, byGroup.get(group)]));

  for (const [group, ops] of sorted) writeFileSync(join(out, `${group}.mdx`), groupPage(group, ops, program, schemas));
  writeFileSync(join(out, 'index.mdx'), indexPage(program, root, sorted));
  writeFileSync(
    join(out, 'meta.json'),
    JSON.stringify({ title: program.title, defaultOpen: true, pages: order }, null, 2) + '\n',
  );
  console.log(`wrote ${root.operations.length} operations in ${order.length} pages to api/${program.dir}`);
}

// ---------------------------------------------------------------------------
// The server's methods, from api/methods.json.

const methodGroups = {
  access: ['Access', 'What the caller holds, and grants on resources.'],
  admin: ['Administration', 'Accounts, tokens and what only an admin does.'],
  audit: ['Audit', 'What was done, by whom.'],
  auth: ['Sign-in', 'Who the caller is, sign-in links and sessions.'],
  blobs: ['Blobs', 'Files the server keeps by their hash.'],
  connections: ['Connections', 'Credentials for other services, of a declared type.'],
  env: ['Environments', 'Saved hosts, their presence, files, TCP ports and forwarded ports.'],
  events: ['Events', 'One stream of what changes, for whoever may see it.'],
  hosts: ['Hosts', "The hosts, their profiles and actions, their sessions, files and ports, messages and channel."],
  installation: ['Installation', "The installation's actions, secrets and settings."],
  jobs: ['Jobs', 'Long-running work, such as an apply, with its log.'],
  keys: ['Keys', 'The key ring Rookery logs in to hosts with.'],
  layout: ['Layout', "The shell's navigation."],
  leases: ['Leases', 'Named locks that end unless renewed.'],
  notify: ['Notifications', 'Messages to people.'],
  plugins: ['Plugins', 'The plugins the server has.'],
  provisioners: ['Provisioners', 'What machines and deployments are made with.'],
  registries: ['Registries', 'Package registries and their logins.'],
  resources: ['Resources', 'What provisioners made: creating, changing, removing, actions, logs and endpoints.'],
  secrets: ['Secrets', "Plugins' secret values."],
  settings: ['Settings', "Plugins' settings."],
  setup: ['Set-up', 'What the set-up of a new host starts from.'],
  storage: ['Storage', 'Small JSON values with revisions.'],
  streams: ['Streams', 'Ending and crediting the binary streams of `/ws`.'],
  terminals: ['Terminals', 'Interactive terminals on hosts, their viewers, size and screen.'],
  topics: ['Topics', 'Messages published on topics, and following them.'],
};

function levelText(level) {
  switch (level.kind) {
    case 'public':
      return 'Anyone, signed in or not';
    case 'self':
      return 'Any signed-in caller, for itself';
    case 'admin':
      return 'An admin';
    case 'plugin':
      return 'A plugin';
    case 'resource': {
      const scheme = level.scheme ? ` (\`${level.scheme}://NAME\`, or the bare name)` : '';
      return `\`${level.grant}\` on the resource in \`${level.param}\`${scheme}`;
    }
    default:
      return level.kind;
  }
}

function schemaType(node, defs) {
  if (!node || node === true) return 'any';
  if (node.$ref) return `\`${node.$ref.split('/').pop()}\``;
  if (node.enum) return node.enum.map((v) => `\`${JSON.stringify(v)}\``).join(' \\| ');
  if (node.const !== undefined) return `\`${JSON.stringify(node.const)}\``;
  for (const key of ['anyOf', 'oneOf']) {
    if (node[key]) {
      const parts = node[key].filter((n) => n.type !== 'null').map((n) => schemaType(n, defs));
      return parts.join(' or ') + (node[key].some((n) => n.type === 'null') ? ', or null' : '');
    }
  }
  if (node.allOf?.length === 1) return schemaType(node.allOf[0], defs);
  const type = Array.isArray(node.type) ? node.type.filter((t) => t !== 'null').join(' or ') : node.type;
  if (type === 'array') return `list of ${schemaType(node.items, defs)}`;
  if (type === 'object' && node.additionalProperties && node.additionalProperties !== true && !node.properties)
    return `map of ${schemaType(node.additionalProperties, defs)}`;
  return type ?? 'any';
}

function propertyTable(node, defs) {
  const props = node?.properties;
  if (!props || !Object.keys(props).length) return '';
  const required = new Set(node.required ?? []);
  let text = '| Field | Type | Description |\n| --- | --- | --- |\n';
  for (const [name, prop] of Object.entries(props)) {
    text += `| \`${name}\`${required.has(name) ? '' : ' (optional)'} | ${cell(schemaType(prop, defs))} | ${cell(prop.description ?? '')} |\n`;
  }
  return text + '\n';
}

function methodSection(method) {
  let text = `## \`${method.name}\`\n\n${md(method.summary ?? '')}\n\n`;
  text += `Who may call: ${levelText(method.level)}.${method.stream ? ' It opens a stream.' : ''}\n\n`;
  for (const [title, node] of [['Parameters', method.params], ['Result', method.result]]) {
    if (!node || node === true) continue;
    const defs = node.$defs ?? {};
    text += `### ${title} [#${anchor(`${method.name} ${title}`)}]\n\n`;
    if (node.description) text += `${md(node.description)}\n\n`;
    const table = propertyTable(node, defs);
    if (table) text += table;
    else if (!node.description) text += `${schemaType(node, defs)}\n\n`;
    if (Object.keys(defs).length) {
      text += `<details>\n<summary>The schema</summary>\n\n\`\`\`json\n${json(node)}\n\`\`\`\n\n</details>\n\n`;
    }
  }
  return text;
}

{
  const methods = JSON.parse(readFileSync(join(docs, '..', 'api', 'methods.json'), 'utf8'));
  const out = join(docs, 'content/docs/api', 'server');
  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });
  const byGroup = new Map();
  for (const method of methods) {
    const group = method.name.split('.')[0];
    if (!byGroup.has(group)) byGroup.set(group, []);
    byGroup.get(group).push(method);
  }
  const order = [...byGroup.keys()].sort();
  let index = '---\ntitle: rookery serve\ndescription: "The methods of the server, on /ws and /rpc."\n---\n\n';
  index += 'Each method is called as JSON-RPC 2.0 on `/ws` or `POST /rpc`, or on the server\'s Unix socket. See the [overview](/docs/api).\n\n';
  index += '| Method | Summary |\n| --- | --- |\n';
  for (const group of order) {
    const [title, description] = methodGroups[group] ?? [group, ''];
    let page = `---\ntitle: ${yaml(title)}\ndescription: ${yaml(description)}\n---\n\n`;
    page += '| Method | Who may call |\n| --- | --- |\n';
    for (const method of byGroup.get(group)) {
      page += `| [\`${method.name}\`](#${anchor(method.name)}) | ${cell(levelText(method.level))} |\n`;
      index += `| [\`${method.name}\`](/docs/api/server/${group}#${anchor(method.name)}) | ${cell(method.summary ?? '')} |\n`;
    }
    page += '\n';
    for (const method of byGroup.get(group)) page += methodSection(method);
    writeFileSync(join(out, `${group}.mdx`), page);
  }
  writeFileSync(join(out, 'index.mdx'), index + '\n');
  writeFileSync(
    join(out, 'meta.json'),
    JSON.stringify({ title: 'rookery serve', defaultOpen: true, pages: order }, null, 2) + '\n',
  );
  console.log(`wrote ${methods.length} methods in ${order.length} pages to api/server`);
}
