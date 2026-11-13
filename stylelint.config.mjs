import { propertyGroups } from 'stylelint-config-clean-order';

/*
 * Stylelint covers real CSS files only (`src/ui/src/main.css`). StyleX style
 * objects are JavaScript and are linted by oxlint with the matching rules:
 * `@stylexjs/sort-keys` (`order: 'clean'`), `@stylexjs/valid-styles` and the
 * local `stache-stylex/no-low-performance-animation-properties`.
 */

const propertiesOrder = propertyGroups.map((properties) => ({
  emptyLineBefore: 'always',
  noEmptyLineBetween: true,
  properties,
}));

/* Find transition/animation properties and add will-change to them */
propertiesOrder
  .find((group) => ['transition', 'animation'].some((prop) => group.properties.includes(prop)))
  ?.properties.push('will-change');

/** @type {import('stylelint').Config} */
export default {
  extends: ['stylelint-config-recommended', 'stylelint-config-clean-order'],
  plugins: ['stylelint-high-performance-animation'],
  rules: {
    'nesting-selector-no-missing-scoping-root': null,
    'property-no-vendor-prefix': null,
    'plugin/no-low-performance-animation-properties': [
      true,
      {
        ignore: 'paint-properties',
      },
    ],
    'selector-pseudo-element-no-unknown': true,
    'comment-empty-line-before': [
      'always',
      {
        // oxfmt removes the blank line before a comment that opens a block.
        except: ['first-nested'],
        ignore: ['stylelint-commands', 'after-comment'],
      },
    ],
    'order/properties-order': [
      propertiesOrder,
      {
        severity: 'error',
        unspecified: 'bottomAlphabetical',
      },
    ],
  },
};
