export default {
  '*.{js,ts,tsx,json,jsonc,css,md,yaml}': ['prettier --write'],
  '*.{js,ts,tsx}': ['eslint --fix'],
  '**/*.(styles.ts|css)': ['stylelint --fix --allow-empty-input'],
};
