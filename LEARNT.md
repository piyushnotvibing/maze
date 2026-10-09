# Parse, Use, Drop (Storage Engine Concept)
- conventional wisdom tells us to list a field corresponding to a data item from a data store, like while building backends, you create a Users struct with all the fields corresponding to all the columns in the user table in the database
- but this is not necessary when parsing raw bytes out of the disk
- some information is purely metadata; they are signals as to how many bytes one should read in order to correctly parse the data out of the disk.
    - like, "hey im size, my value tells you how many bytes to read to correctly parse the string that immediately follows me"
    - this metadata is only useful for parsing the string. after parsing, we can throw it away, we don't need a Cell struct with size, data fields
    - no, we only need the data.
- i'll define a convention that data travelling from the disk to the memory (parsing) is direction UP, and data travelling from memory to disk (serializing) is direction DOWN
    1. UP:
        - when parsing from the disk, we parse the metadata, store it locally, temporarily, use it to parse the actual data, and then drop it. the structs we define as the namesake of the logical disk structures only store the actual data, not the metadata.
    2. DOWN:
        - when serializing to the disk, we construct the metadata from the data we want to write down, and construct the logical disk structures in memory before saving it onto the disk.

- tests should follow `parse(serialize(page)) == page` and `serialize(parse(buf)) may or may not be equal to buf` (if `buf` was already in correct packed layout, ie, the cell pointers are sorted by the pointed cell's row ids, there are no gaps between cells (gaps will be introduced when a cell (row) is deleted by the user, then `serialize(parse(buf))` will remove the fragmented bytes))
