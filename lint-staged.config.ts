export default {
  '*.{js,ts,tsx,json,jsonc,css,md,yaml,yml}': ['oxfmt'],
  '*.{js,ts,tsx}': ['oxlint --fix'],
  '**/*.(styles.ts|css)': ['stylelint --fix --allow-empty-input'],
};
