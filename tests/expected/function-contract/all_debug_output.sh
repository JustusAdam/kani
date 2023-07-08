INSTRUMENT=~/research/cbmc/build/bin/goto-instrument

FILE=$1

FILEBASE=${FILE/%.*}

$INSTRUMENT --show-symbol-table $FILE > $FILEBASE.symtab
$INSTRUMENT --print-internal-representation $FILE > $FILEBASE.ireps
$INSTRUMENT --dump-c $FILE > $FILEBASE.recovered.c