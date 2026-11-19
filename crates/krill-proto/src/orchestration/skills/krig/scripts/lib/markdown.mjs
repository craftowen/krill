export function updateFenceState(fenceState, line) {
  const trimmed = line.trim();
  const match = /^(`{3,}|~{3,})(.*)$/.exec(trimmed);
  if (!match) return { toggled: false, nextState: fenceState };

  const marker = match[1];
  const rest = match[2];
  const char = marker[0];
  const length = marker.length;

  if (!fenceState) {
    if (char === '`' && rest.includes('`')) {
      return { toggled: false, nextState: null };
    }
    return { toggled: true, nextState: { char, length } };
  }

  if (fenceState.char === char && length >= fenceState.length && rest.trim() === '') {
    return { toggled: true, nextState: null };
  }

  return { toggled: false, nextState: fenceState };
}

export function stripInlineCode(line) {
  return line.replace(/(`+)(?:(?!\1).)+\1/g, '');
}

function tokenizeBlocks(markdown) {
  const lines = markdown.split(/\r?\n/);
  const proseWords = [];
  const fencedBlocks = [];
  let fence = null;
  let fenceBuf = [];

  for (const line of lines) {
    const step = updateFenceState(fence, line);
    if (step.toggled) {
      if (fence && !step.nextState) {
        fencedBlocks.push(fenceBuf.join('\n'));
        fenceBuf = [];
      }
      fence = step.nextState;
      continue;
    }
    if (fence) {
      fenceBuf.push(line);
    } else {
      proseWords.push(...line.trim().split(/\s+/).filter(Boolean));
    }
  }
  return { proseWords, fencedBlocks };
}

function isStructuralMarkdownLine(line) {
  const trimmed = line.trim();
  if (!trimmed) return true;
  if (/^\s{2,}\S/.test(line)) return true;
  return (
    /^#{1,6}\s/.test(trimmed)
    || /^[-*+]\s/.test(trimmed)
    || /^\d+\.\s/.test(trimmed)
    || /^>\s?/.test(trimmed)
    || /^\|/.test(trimmed)
    || /^---+$/.test(trimmed)
  );
}

export function reflowMarkdown(markdown) {
  const lines = markdown.split(/\r?\n/);
  const out = [];
  let inFrontmatter = false;
  let fence = null;
  let paragraphBuf = [];

  function flushParagraph() {
    if (paragraphBuf.length > 0) {
      out.push(paragraphBuf.join(' '));
      paragraphBuf = [];
    }
  }

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    if (i === 0 && line.trim() === '---') {
      inFrontmatter = true;
      out.push(line);
      continue;
    }
    if (inFrontmatter) {
      out.push(line);
      if (line.trim() === '---') inFrontmatter = false;
      continue;
    }

    const step = updateFenceState(fence, line);
    if (step.toggled) {
      flushParagraph();
      fence = step.nextState;
      out.push(line);
      continue;
    }
    if (fence) {
      out.push(line);
      continue;
    }

    if (isStructuralMarkdownLine(line)) {
      flushParagraph();
      out.push(line);
    } else {
      paragraphBuf.push(line.trim());
    }
  }
  flushParagraph();

  const result = out.join('\n');
  const before = tokenizeBlocks(markdown);
  const after = tokenizeBlocks(result);

  if (
    before.proseWords.join(' ') !== after.proseWords.join(' ')
    || JSON.stringify(before.fencedBlocks) !== JSON.stringify(after.fencedBlocks)
  ) {
    throw new Error('무손실 검증 실패: 재정렬 전후 단어 토큰 또는 코드 블록이 일치하지 않습니다.');
  }

  return result;
}
