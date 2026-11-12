/*
 * A local lint plugin for StyleX style objects. It carries over
 * `stylelint-high-performance-animation`'s
 * `no-low-performance-animation-properties` with `ignore: 'paint-properties'`:
 * transitions and keyframes must not animate a property that triggers layout.
 * Stylelint cannot read StyleX objects, so the rule lives here.
 */

/** Properties whose animation triggers layout (the stylelint plugin's list). */
const LAYOUT_PROPERTIES = new Set([
  'position',
  'top',
  'bottom',
  'left',
  'right',
  'inset-block-start',
  'inset-block-end',
  'inset-inline-start',
  'inset-inline-end',
  'width',
  'height',
  'inline-size',
  'block-size',
  'min-height',
  'max-height',
  'max-width',
  'min-width',
  'min-block-size',
  'max-block-size',
  'min-inline-size',
  'max-inline-size',
  'padding',
  'padding-bottom',
  'padding-left',
  'padding-right',
  'padding-top',
  'padding-block-end',
  'padding-inline-start',
  'padding-inline-end',
  'padding-block-start',
  'margin',
  'margin-bottom',
  'margin-left',
  'margin-right',
  'margin-top',
  'margin-block-end',
  'margin-inline-start',
  'margin-inline-end',
  'margin-block-start',
  'display',
  'border-width',
  'border-spacing',
  'border-collapse',
  'border',
  'font',
  'font-size',
  'font-family',
  'font-weight',
  'font-style',
  'float',
  'overflow-y',
  'overflow-x',
  'overflow',
  'overflow-block',
  'overflow-inline',
  'line-height',
  'vertical-align',
  'clear',
  'white-space',
  'list-style',
  'list-style-type',
  'zoom',
  'content',
  'box-sizing',
  'text-shadow',
  'text-align',
  'text-indent',
  'text-transform',
  'text-overflow',
  'word-wrap',
  'letter-spacing',
  'appearance',
  'direction',
]);

const TIMING_FUNCTION =
  /^(?:linear|ease|ease-in|ease-out|ease-in-out|cubic-bezier|step-start|step-end|steps|frames)/;
const NUMBER_OR_UNIT = /^[+-]?(?:\d+\.?\d*|\.\d+)(?:e[+-]?\d+)?[a-z%]*$/i;

const unprefixed = (property) => property.replace(/^-\w+-/, '');

const kebab = (name) =>
  name.startsWith('--') ? name : name.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);

const isLowPerformance = (word) => {
  const property = unprefixed(word);

  return property === 'all' || LAYOUT_PROPERTIES.has(property);
};

/** Splits a CSS value on whitespace and commas, keeping `fn(...)` whole. */
function words(value) {
  const found = [];
  let depth = 0;
  let current = '';

  for (const character of value) {
    if (character === '(') {
      depth += 1;
    } else if (character === ')') {
      depth = Math.max(0, depth - 1);
    }

    if (depth === 0 && /[\s,]/.test(character)) {
      if (current) {
        found.push(current);
      }

      current = '';
    } else {
      current += character;
    }
  }

  if (current) {
    found.push(current);
  }

  return found;
}

/** Splits a CSS value on its top-level commas (one entry per transition). */
function segments(value) {
  const found = [];
  let depth = 0;
  let current = '';

  for (const character of value) {
    if (character === '(') {
      depth += 1;
    } else if (character === ')') {
      depth = Math.max(0, depth - 1);
    }

    if (depth === 0 && character === ',') {
      found.push(current);
      current = '';
    } else {
      current += character;
    }
  }

  found.push(current);

  return found.filter((segment) => segment.trim() !== '');
}

const keyName = (property) => {
  if (property.type !== 'Property' || property.computed) {
    return null;
  }

  if (property.key.type === 'Identifier') {
    return property.key.name;
  }

  return typeof property.key.value === 'string' ? property.key.value : null;
};

/**
 * Every statically known string of a style value. A conditional value
 * (`{ default: ..., ':hover': ... }`) contributes each branch; a template
 * literal contributes its text with expressions (token references) left out.
 */
function staticStrings(node, out = []) {
  if (!node) {
    return out;
  }

  if (node.type === 'Literal' && typeof node.value === 'string') {
    out.push({ node, text: node.value });
  } else if (node.type === 'TemplateLiteral') {
    out.push({ node, text: node.quasis.map((quasi) => quasi.value.cooked ?? '').join(' ') });
  } else if (node.type === 'ObjectExpression') {
    for (const property of node.properties) {
      if (property.type === 'Property') {
        staticStrings(property.value, out);
      }
    }
  } else if (node.type === 'ConditionalExpression') {
    staticStrings(node.consequent, out);
    staticStrings(node.alternate, out);
  } else if (node.type === 'TSAsExpression' || node.type === 'TSSatisfiesExpression') {
    staticStrings(node.expression, out);
  }

  return out;
}

const isKeyframesCall = (node) => {
  const { callee } = node;

  if (callee.type === 'Identifier') {
    return callee.name === 'keyframes';
  }

  return (
    callee.type === 'MemberExpression' &&
    !callee.computed &&
    callee.property.type === 'Identifier' &&
    callee.property.name === 'keyframes'
  );
};

const noLowPerformanceAnimationProperties = {
  meta: {
    type: 'problem',
    docs: {
      description: 'Disallow transitions and keyframes that animate a layout-triggering property.',
    },
    messages: {
      rejected: 'Unexpected use of low performance {{type}} property ({{property}}).',
    },
    schema: [],
  },
  create(context) {
    const report = (node, type, property) => {
      context.report({ node, messageId: 'rejected', data: { type, property } });
    };

    const checkTransitionProperty = (value) => {
      for (const { node, text } of staticStrings(value)) {
        for (const word of words(text)) {
          // StyleX dashifies camelCase words here (`marginTop` -> `margin-top`).
          if (isLowPerformance(kebab(word))) {
            report(node, 'transition', word);
          }
        }
      }
    };

    const checkTransition = (value) => {
      for (const { node, text } of staticStrings(value)) {
        for (const segment of segments(text)) {
          const all = words(segment);
          const properties = all.filter(
            (word) => !NUMBER_OR_UNIT.test(word) && !TIMING_FUNCTION.test(word),
          );

          // Only durations and easings: this transition applies to `all`. A
          // template literal may get its property from an expression instead.
          if (all.length > 0 && properties.length === 0 && node.type === 'Literal') {
            report(node, 'transition', 'all');
          }

          for (const word of properties) {
            if (isLowPerformance(word)) {
              report(node, 'transition', word);
            }
          }
        }
      }
    };

    return {
      Property(node) {
        const name = keyName(node);

        if (name === 'transitionProperty') {
          checkTransitionProperty(node.value);

          return;
        }

        if (name === 'transition') {
          const siblings = node.parent.type === 'ObjectExpression' ? node.parent.properties : [];

          // An explicit `transitionProperty` in the same style decides instead.
          if (!siblings.some((sibling) => keyName(sibling) === 'transitionProperty')) {
            checkTransition(node.value);
          }
        }
      },
      CallExpression(node) {
        const [frames] = node.arguments;

        if (!isKeyframesCall(node) || frames?.type !== 'ObjectExpression') {
          return;
        }

        for (const frame of frames.properties) {
          if (frame.type !== 'Property' || frame.value.type !== 'ObjectExpression') {
            continue;
          }

          for (const declaration of frame.value.properties) {
            const name = keyName(declaration);

            if (name && LAYOUT_PROPERTIES.has(unprefixed(kebab(name)))) {
              report(declaration.key, 'animation', kebab(name));
            }
          }
        }
      },
    };
  },
};

export default {
  meta: { name: 'stache-stylex' },
  rules: {
    'no-low-performance-animation-properties': noLowPerformanceAnimationProperties,
  },
};
