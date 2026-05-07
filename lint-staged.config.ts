const formatWithOxfmt = (files: string[]) => {
  const supportedFiles = files.filter(
    (file) => !file.startsWith('app/native/') && !file.includes('/app/native/'),
  );

  return supportedFiles.length === 0
    ? []
    : `oxfmt ${supportedFiles.map((file) => JSON.stringify(file)).join(' ')}`;
};

export default {
  '*.{js,ts,tsx,json,jsonc,css,md,yaml,yml}': formatWithOxfmt,
  '*.{js,ts,tsx}': ['oxlint --fix'],
  '**/*.(styles.ts|css)': ['stylelint --fix --allow-empty-input'],
};
