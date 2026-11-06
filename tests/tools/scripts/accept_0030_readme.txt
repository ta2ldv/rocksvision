# 0030 phase 2 DRAFT README walkthroughs 1-3 performed literally (labels and shortcuts must match the README text).
# Walkthrough 1: Open Folder (path bar, since a native dialog cannot be driven) -> examples/data/multilevel-advanced-db, LSM Map (Shift+Cmd+M),
#   CF selector, select a box / WAL card, double-click / Enter opens, Back.
# Walkthrough 2: examples/files/sst-zstd/sst-zstd.sst: Structure tree data block -> entry, File bytes returns.
# Walkthrough 3: examples/files/wal-plain/000004.log: Physical blocks, Logical records, WriteBatch -> operation.
# Placeholders: EXAMPLES (default provided by the driver), PATHX PATHY GOX GOY, CFSELX CFSELY (CF selector), CFITEMX CFITEMY (second CF), BOXX BOXY (L1 box), WALX WALY,
#   BACKX BACKY, TREE_BLOCKX TREE_BLOCKY, TREE_ENTRYX TREE_ENTRYY, FILEBYTESX FILEBYTESY, PHYSX PHYSY, LOGICALX LOGICALY, BATCHX BATCHY, OPX OPY, PARKX PARKY
front
note W1 Open Folder -> multilevel-advanced-db
click {{PATHX}} {{PATHY}}
key a cmd
type {{EXAMPLES}}/data/multilevel-advanced-db
click {{GOX}} {{GOY}}
sleep 2.5
key m cmd shift
sleep 1.5
move {{PARKX}} {{PARKY}}
shot w1_1_map
click {{CFSELX}} {{CFSELY}}
sleep 0.6
shot w1_2_cf_selector
click {{CFITEMX}} {{CFITEMY}}
sleep 1.0
click {{BOXX}} {{BOXY}}
sleep 0.8
move {{PARKX}} {{PARKY}}
shot w1_3_box_selected
key return
sleep 1.2
shot w1_4_opened
click {{BACKX}} {{BACKY}}
sleep 1.0
shot w1_5_back
note W2 Open File -> sst-zstd
click {{PATHX}} {{PATHY}}
key a cmd
type {{EXAMPLES}}/files/sst-zstd/sst-zstd.sst
click {{GOX}} {{GOY}}
sleep 2.0
click {{TREE_BLOCKX}} {{TREE_BLOCKY}}
sleep 0.8
shot w2_1_block
click {{TREE_ENTRYX}} {{TREE_ENTRYY}}
sleep 0.8
move {{PARKX}} {{PARKY}}
shot w2_2_entry
click {{FILEBYTESX}} {{FILEBYTESY}}
sleep 0.6
shot w2_3_file_bytes
note W3 Open File -> wal-plain/000004.log
click {{PATHX}} {{PATHY}}
key a cmd
type {{EXAMPLES}}/files/wal-plain/000004.log
click {{GOX}} {{GOY}}
sleep 2.0
click {{PHYSX}} {{PHYSY}}
sleep 0.8
shot w3_1_physical
click {{LOGICALX}} {{LOGICALY}}
sleep 0.8
shot w3_2_logical
click {{BATCHX}} {{BATCHY}}
sleep 0.8
click {{OPX}} {{OPY}}
sleep 0.8
move {{PARKX}} {{PARKY}}
shot w3_3_operation
