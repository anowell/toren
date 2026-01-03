import * as path from 'path';

/**
 * Detect segment name from current directory or git/jj repo
 */
export function detectSegmentName(): string {
  const cwd = process.cwd();
  const dirName = path.basename(cwd);

  // Capitalize first letter for segment naming convention
  return capitalize(dirName);
}

/**
 * Convert number to word form (One, Two, Three, etc.)
 * Following Ancillary Justice naming: One Esk Nineteen, Kalr Five
 */
export function numberToWord(n: number): string {
  const words = [
    'Zero', 'One', 'Two', 'Three', 'Four', 'Five',
    'Six', 'Seven', 'Eight', 'Nine', 'Ten',
    'Eleven', 'Twelve', 'Thirteen', 'Fourteen', 'Fifteen',
    'Sixteen', 'Seventeen', 'Eighteen', 'Nineteen', 'Twenty'
  ];

  if (n >= 0 && n < words.length) {
    return words[n];
  }

  // For numbers > 20, just use numeric
  return n.toString();
}

/**
 * Parse ancillary number from word form back to number
 */
export function wordToNumber(word: string): number {
  const words = [
    'zero', 'one', 'two', 'three', 'four', 'five',
    'six', 'seven', 'eight', 'nine', 'ten',
    'eleven', 'twelve', 'thirteen', 'fourteen', 'fifteen',
    'sixteen', 'seventeen', 'eighteen', 'nineteen', 'twenty'
  ];

  const index = words.indexOf(word.toLowerCase());
  return index >= 0 ? index : parseInt(word);
}

/**
 * Get next available ancillary number for a segment
 */
export function getNextAncillaryNumber(existingAncillaries: string[]): string {
  // Extract numbers from existing ancillaries
  const numbers = existingAncillaries
    .map(a => {
      const parts = a.split(' ');
      const lastPart = parts[parts.length - 1];
      return wordToNumber(lastPart);
    })
    .filter(n => !isNaN(n));

  const maxNumber = numbers.length > 0 ? Math.max(...numbers) : 0;
  return numberToWord(maxNumber + 1);
}

function capitalize(str: string): string {
  return str.charAt(0).toUpperCase() + str.slice(1);
}

/**
 * Validate ancillary name format
 * Valid: "Howie One", "Agent Two", "MyProject Three"
 */
export function validateAncillaryName(name: string): boolean {
  const parts = name.trim().split(' ');
  if (parts.length < 2) return false;

  const number = parts[parts.length - 1];
  // Check if last part is a valid number word or digit
  return wordToNumber(number) >= 0;
}

/**
 * Format full ancillary name
 */
export function formatAncillaryName(segmentName: string, number: string | number): string {
  const numberStr = typeof number === 'number' ? numberToWord(number) : number;
  return `${capitalize(segmentName)} ${numberStr}`;
}
