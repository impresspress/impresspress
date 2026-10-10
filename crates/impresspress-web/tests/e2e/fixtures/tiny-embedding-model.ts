/**
 * A tiny stand-in for an embedding model's files on the Hugging Face hub, for
 * running the REAL Transformers.js and ONNX Runtime in a test without
 * downloading a real model (the runtime's default, `multilingual-e5-small`, is
 * over 100 MB).
 *
 * The files are what Transformers.js's `pipeline('feature-extraction', …)`
 * reads for a BERT-type model: `config.json`, `tokenizer.json`,
 * `tokenizer_config.json` and the ONNX graph `onnx/model_quantized.onnx` (the
 * `q8` file `embed-engine.js` asks for). The graph is real ONNX, built below,
 * and ONNX Runtime runs it: its `last_hidden_state` is
 * `attention_mask[b, s] * WEIGHTS[d]`, so after the pipeline's mean pooling
 * and normalization every text's vector is `WEIGHTS / |WEIGHTS|` — a value
 * only a model run produces, and one a test can check exactly.
 */

/** Width of the vectors: the runtime's default embedding model's (`multilingual-e5-small`). */
export const DIMS = 384;

/** The per-dimension weights the graph multiplies the attention mask by: 1, 2, …, DIMS. */
export const WEIGHTS = Array.from({ length: DIMS }, (_, d) => d + 1);

/** The vector the pipeline returns for every text: `WEIGHTS`, normalized. */
export function expectedVector(): number[] {
  const norm = Math.hypot(...WEIGHTS);
  return WEIGHTS.map((w) => w / norm);
}

// ---------------------------------------------------------------------------
// Protocol-buffer encoding of the few ONNX messages the graph needs
// (onnx/onnx.proto: ModelProto, GraphProto, NodeProto, AttributeProto,
// TensorProto, ValueInfoProto, TypeProto, TensorShapeProto).
// ---------------------------------------------------------------------------

type Bytes = number[];

function varint(n: number): Bytes {
  const out: Bytes = [];
  let v = BigInt(n);
  do {
    let byte = Number(v & 0x7fn);
    v >>= 7n;
    if (v > 0n) byte |= 0x80;
    out.push(byte);
  } while (v > 0n);
  return out;
}

/** A varint field. */
const int = (field: number, n: number): Bytes => [...varint(field << 3), ...varint(n)];

/** A length-delimited field: a string, raw bytes or an embedded message. */
function bytes(field: number, payload: Bytes | string): Bytes {
  const body = typeof payload === 'string' ? [...new TextEncoder().encode(payload)] : payload;
  return [...varint((field << 3) | 2), ...varint(body.length), ...body];
}

const FLOAT = 1;
const INT64 = 7;
const ATTRIBUTE_INT = 2;

function tensor(name: string, dataType: number, dims: number[], raw: Uint8Array): Bytes {
  return [...dims.flatMap((d) => int(1, d)), ...int(2, dataType), ...bytes(8, name), ...bytes(9, [...raw])];
}

/** A tensor-typed graph input or output; a string dimension is symbolic. */
function valueInfo(name: string, elemType: number, shape: (number | string)[]): Bytes {
  const dims = shape.map((d) => bytes(1, typeof d === 'number' ? int(1, d) : bytes(2, d)));
  const tensorType = [...int(1, elemType), ...bytes(2, dims.flat())];
  return [...bytes(1, name), ...bytes(2, bytes(1, tensorType))];
}

function node(op: string, inputs: string[], outputs: string[], attributes: Bytes[] = []): Bytes {
  return [
    ...inputs.flatMap((i) => bytes(1, i)),
    ...outputs.flatMap((o) => bytes(2, o)),
    ...bytes(3, `${op.toLowerCase()}_${outputs[0]}`),
    ...bytes(4, op),
    ...attributes.flatMap((a) => bytes(5, a)),
  ];
}

/** `onnx/model_quantized.onnx`: last_hidden_state = attention_mask (as float, unsqueezed) * WEIGHTS. */
export function onnxModel(): Buffer {
  const axes = new Uint8Array(new BigInt64Array([2n]).buffer);
  const weights = new Uint8Array(new Float32Array(WEIGHTS).buffer);
  const graph = [
    ...bytes(1, node('Cast', ['attention_mask'], ['mask_f'], [[...bytes(1, 'to'), ...int(3, FLOAT), ...int(20, ATTRIBUTE_INT)]])),
    ...bytes(1, node('Unsqueeze', ['mask_f', 'axes'], ['mask_3d'])),
    ...bytes(1, node('Mul', ['mask_3d', 'weights'], ['last_hidden_state'])),
    ...bytes(2, 'tiny-embedding'),
    ...bytes(5, tensor('axes', INT64, [1], axes)),
    ...bytes(5, tensor('weights', FLOAT, [DIMS], weights)),
    ...bytes(11, valueInfo('input_ids', INT64, ['batch', 'sequence'])),
    ...bytes(11, valueInfo('attention_mask', INT64, ['batch', 'sequence'])),
    ...bytes(12, valueInfo('last_hidden_state', FLOAT, ['batch', 'sequence', DIMS])),
  ];
  const model = [
    ...int(1, 8), // ir_version
    ...bytes(2, 'impresspress-e2e'), // producer_name
    ...bytes(7, graph),
    ...bytes(8, [...bytes(1, ''), ...int(2, 13)]), // opset_import: the default domain, opset 13
  ];
  return Buffer.from(model);
}

// ---------------------------------------------------------------------------
// The tokenizer: BERT's WordPiece over a handful of words; anything else is
// `[UNK]`, which is all a graph that reads only the attention mask needs.
// ---------------------------------------------------------------------------

const VOCAB = ['[PAD]', '[UNK]', '[CLS]', '[SEP]', 'hello', 'world'];
const id = (token: string) => VOCAB.indexOf(token);

function tokenizerJson(): string {
  const special = (token: string) => ({
    id: id(token),
    content: token,
    single_word: false,
    lstrip: false,
    rstrip: false,
    normalized: false,
    special: true,
  });
  return JSON.stringify({
    version: '1.0',
    truncation: null,
    padding: null,
    added_tokens: ['[PAD]', '[UNK]', '[CLS]', '[SEP]'].map(special),
    normalizer: {
      type: 'BertNormalizer',
      clean_text: true,
      handle_chinese_chars: true,
      strip_accents: null,
      lowercase: true,
    },
    pre_tokenizer: { type: 'BertPreTokenizer' },
    post_processor: {
      type: 'TemplateProcessing',
      single: [
        { SpecialToken: { id: '[CLS]', type_id: 0 } },
        { Sequence: { id: 'A', type_id: 0 } },
        { SpecialToken: { id: '[SEP]', type_id: 0 } },
      ],
      pair: [
        { SpecialToken: { id: '[CLS]', type_id: 0 } },
        { Sequence: { id: 'A', type_id: 0 } },
        { SpecialToken: { id: '[SEP]', type_id: 0 } },
        { Sequence: { id: 'B', type_id: 1 } },
        { SpecialToken: { id: '[SEP]', type_id: 1 } },
      ],
      special_tokens: {
        '[CLS]': { id: '[CLS]', ids: [id('[CLS]')], tokens: ['[CLS]'] },
        '[SEP]': { id: '[SEP]', ids: [id('[SEP]')], tokens: ['[SEP]'] },
      },
    },
    decoder: { type: 'WordPiece', prefix: '##', cleanup: true },
    model: {
      type: 'WordPiece',
      unk_token: '[UNK]',
      continuing_subword_prefix: '##',
      max_input_chars_per_word: 100,
      vocab: Object.fromEntries(VOCAB.map((token, i) => [token, i])),
    },
  });
}

/** The model's files by their path under the repository's `resolve/<revision>/`. */
export function modelFiles(): Record<string, { contentType: string; body: Buffer }> {
  const json = (value: unknown) => ({
    contentType: 'application/json',
    body: Buffer.from(typeof value === 'string' ? value : JSON.stringify(value)),
  });
  return {
    'config.json': json({
      model_type: 'bert',
      architectures: ['BertModel'],
      hidden_size: DIMS,
    }),
    'tokenizer.json': json(tokenizerJson()),
    'tokenizer_config.json': json({
      tokenizer_class: 'BertTokenizer',
      do_lower_case: true,
      model_max_length: 512,
      cls_token: '[CLS]',
      sep_token: '[SEP]',
      pad_token: '[PAD]',
      unk_token: '[UNK]',
    }),
    'onnx/model_quantized.onnx': { contentType: 'application/octet-stream', body: onnxModel() },
  };
}
