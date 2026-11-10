import { helpText, parseBarCommandLine, UsageError } from './measure-bar/options';
import { InterruptedError, runBarMeasurement } from './measure-bar/run';

async function main(): Promise<number> {
  let parsed: ReturnType<typeof parseBarCommandLine>;

  try {
    parsed = parseBarCommandLine(process.argv.slice(2));
  } catch (error) {
    if (error instanceof UsageError) {
      process.stderr.write(`measure:bar: ${error.message}\n\n${helpText()}`);

      return 2;
    }

    throw error;
  }

  if (parsed.kind === 'help') {
    process.stdout.write(helpText());

    return 0;
  }

  try {
    const outcome = await runBarMeasurement(parsed.options);

    process.stdout.write(
      `measure:bar: report ${outcome.outputs.reportPath}\n` +
        `measure:bar: samples ${outcome.outputs.samplesPath}\n`,
    );

    if (outcome.failure === null) {
      process.stdout.write(
        `measure:bar: completed; temporary roots ${outcome.clean ? 'removed' : 'NOT all removed (see the report)'}\n`,
      );

      return outcome.clean ? 0 : 1;
    }

    process.stderr.write(
      `measure:bar: ${outcome.interrupted ? 'interrupted' : 'failed'}: ${outcome.failure.message}\n`,
    );

    return outcome.interrupted ? 130 : 1;
  } catch (error) {
    if (error instanceof InterruptedError) {
      process.stderr.write(`measure:bar: interrupted: ${error.message}\n`);

      return 130;
    }

    process.stderr.write(
      `measure:bar: ${error instanceof Error ? error.message : String(error)}\n`,
    );

    return 1;
  }
}

try {
  process.exitCode = await main();
} catch (error) {
  process.stderr.write(`measure:bar: ${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
}
