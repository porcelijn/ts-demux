//
// (c) 2020 Tijn Porcelijn
//

use std::fs::File;
use std::io::{self, Read, BufReader, Write, BufWriter};
use std::collections::HashMap;

const PACKET_SIZE: usize = 188;

type Packet = [u8; PACKET_SIZE];

fn get_pid(packet: &Packet) -> u16 {
    let pid = ((packet[1] & 0x1f) as u16) << 8 | (packet[2] as u16);
    pid
}

fn get_payload_offset(packet: &Packet) -> usize {
    let adaptation_field_control = (packet[3] & 0x30) >> 4;
    match adaptation_field_control {
        0b01 => 4, // only payload, no adaptation field
        0b10 => 188, // only adaptation field, no payload
        0b11 => { // adaptation field followed by payload
            let adaptation_field_length = packet[4] as usize;
            assert!(adaptation_field_length + 5 <= packet.len());
            adaptation_field_length + 5 
        }
        _ => panic!("Invalid adaptation_field_control")
    }
}

fn get_pusi(packet: &Packet) -> bool {
    let pusi = packet[1] & 0x40;
    pusi != 0
}

fn get_continuity_counter(packet: &Packet) -> u8 {
    let continuity_counter = packet[3] & 0x0f;
    continuity_counter
}

fn get_pes_header_size(pes: &[u8]) -> usize {
    assert!(pes.len() >= 9);
    assert_eq!(pes[0], 0x00);
//  assert_eq!(pes[1], 0x00);
//  assert_eq!(pes[2], 0x01);
    let pes_header_length = pes[8] as usize;
    9 + pes_header_length
}

trait PacketProcessor {
    fn process(&mut self, packet: &Packet) -> io::Result<UpdateProgramMap>;
}

type ProgramMap = HashMap<u16, Box<dyn PacketProcessor>>;
type UpdateProgramMap = Box<dyn Fn(&mut ProgramMap)>;
fn no_update() -> UpdateProgramMap {
    Box::new(|_programs: &mut ProgramMap| ())
                            as Box<dyn Fn(&mut ProgramMap)>
}

struct Program {
    continuity_counter: u8,
    writer: Box<dyn Write>
}

impl Program {
    fn new(name: &str, n: usize) -> io::Result<Program> {
        let writer = File::create(name)?;
        let writer = BufWriter::with_capacity(n * PACKET_SIZE, writer);
        let writer = Box::new(writer);

        Ok(Program { continuity_counter: 0, writer })
    }
}

impl PacketProcessor for Program {
    fn process(&mut self, packet: &Packet) -> io::Result<UpdateProgramMap> {
        assert_eq!(packet[0], 0x47);

        // check continuity counter
        let continuity_counter = &mut self.continuity_counter;
        assert_eq!(*continuity_counter, get_continuity_counter(packet));
        *continuity_counter = (*continuity_counter + 1) % 16;

        // skip adaptation field
        let mut offset: usize = get_payload_offset(packet);
  
        // skip PES header
        if get_pusi(packet) {
            offset += get_pes_header_size(&packet[offset..]);
        }

        self.writer.write_all(&packet[offset..])?;

        Ok(no_update())
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        self.writer.flush().unwrap();
    }
}

trait TableProcessor {
    fn process(&mut self, table_data: &[u8]) -> UpdateProgramMap; 
}

struct ProgramAssociationTable {
}

impl TableProcessor for ProgramAssociationTable {
    fn process(&mut self, table_data: &[u8]) -> UpdateProgramMap {
        assert_eq!(table_data.len(), 4);
        let program_number = ((table_data[0] as u16) << 8)
            | (table_data[1] as u16);
        assert_eq!(table_data[2] & 0b11100000, 0b11100000); // reserved bits
        let program_pid = (((table_data[2] & 0x1F) as u16) << 8)
            | (table_data[3] as u16);

        Box::new(move |programs: &mut ProgramMap| {
            programs.entry(program_pid).or_insert_with(|| {
                println!(" PAT: number={}, PMT pid={}", program_number, program_pid);
                let pmt = ProgramMapTable {};
                let psi = ProgramSpecificInformation { table_processor: pmt };
                Box::new(psi)
            });
        })
    }
}

struct ProgramMapTable {
}

impl TableProcessor for ProgramMapTable {
    fn process(&mut self, table_data: &[u8]) -> UpdateProgramMap {
        assert!(table_data.len() > 4);
        assert_eq!(table_data[0] & 0b11100000, 0b11100000); // reserved bits
        let _pcr_pid = (((table_data[0] & 0x1F) as u16) << 8)
            | (table_data[1] as u16);
        assert_eq!(table_data[2] & 0b11111100, 0b11110000); // 4x1 reserved bits + 2x0 unused
        let program_info_length = (((table_data[2] & 0b00000011) as u16) << 8)
            | (table_data[3] as u16);
        let program_info_length = program_info_length  as usize;
        assert!(program_info_length < table_data.len());
        // skip program_descriptor [..]
//      println!(" PMT: pcr_pid={}, program_info_length={}", _pcr_pid, program_info_length);

        let mut es_info_data = &table_data[4 + program_info_length .. ];

        let mut add_programs = no_update();
        while es_info_data.len() >= 5
        {
            // Elementary stream specific data
            let stream_type = es_info_data[0];
            assert_eq!(es_info_data[1] & 0b11100000, 0b11100000); // reserved bits
            let es_pid = (((es_info_data[1] & 0x1F) as u16) << 8)
                | (es_info_data[2] as u16);
            assert_eq!(es_info_data[3] & 0b11111100, 0b11110000); // 4x1 reserved bits + 2x0 unused
            let es_info_length = (((es_info_data[3] & 0b00000011) as u16) << 8)
                | (es_info_data[4] as u16);
            let es_info_length = es_info_length as usize;

            add_programs = Box::new(move |programs: &mut ProgramMap| {
                add_programs(programs);

                programs.entry(es_pid).or_insert_with(|| {
                    let description = match stream_type {
                        0x0F => "ISO/IEC 13818-7 ADTS AAC / MPEG-2 lower bit-rate audio",
                        0x1B => "ISO/IEC 14496-10 / H.264 lower bit-rate video",
                        _  => panic!("unknown stream type")
                    };

                    println!("  ES: stream_type={} ({}), pid={}, length={}", stream_type, description, es_pid, es_info_length);

                    let extension = match stream_type { 0x0F => "aac", 0x1B => "avc",  _ => panic!("unknown stream type") };
                    let filename = format!("elephants-{}.{}", es_pid, extension);
                    let program = Program::new(&filename[..], 100).unwrap();
                    println!("      created: {}", filename);
                    Box::new(program)
                });
            });

            assert!(5 + es_info_length <= es_info_data.len());
            es_info_data = &es_info_data[5 + es_info_length ..];
        }
        assert_eq!(es_info_data.len(), 0);
        add_programs
    }
}

struct ProgramSpecificInformation<T: TableProcessor> {
  table_processor: T
}

impl<T> PacketProcessor for ProgramSpecificInformation<T>
where T: TableProcessor,
{
    fn process(&mut self, packet: &Packet) -> io::Result<UpdateProgramMap>{
        let mut offset: usize = get_payload_offset(packet);

        // skip filler bytes
        if get_pusi(packet) {
            let pointer_field = packet[offset] as usize;
            offset += 1;
            //assert_ne!(pointer_field, 0);
            for filler in &packet[offset .. offset + pointer_field] {
                assert_eq!(*filler, 0xFF);
            }
            offset += pointer_field;
        }

        // Table header
        let table_header = &packet[offset .. offset + 3];
        assert_ne!(table_header[0], 0xFF);
        let _table_id = table_header[0];
        assert_eq!(table_header[1] & 0b11110000, 0b10110000); // section syntax indicator = 1, private bit = 0, reserverd bits = 0x3
        let section_length = ((table_header[1] as u16) & 0x000F) << 8 |
                              (table_header[2] as u16);
        assert!(section_length < 1021);
        let section_length = section_length as usize;
        let _crc_payload = &packet[offset .. offset + 3 + section_length];

//      println!("table_header: id={}, section_length={}", _table_id, section_length);
        offset += 3;

        // Table syntax section
        let table_syntax_section = &packet[offset .. offset + section_length];

        let _table_id_extension = (table_syntax_section[0] as u16) << 8 |
                                  (table_syntax_section[1] as u16);
        assert_eq!(table_syntax_section[2] & 0b11000000, 0b11000000);
        let _syntax_version_number = (table_syntax_section[2] & 0b00111110) >> 1;
        let current_indicator = (table_syntax_section[2] & 0x00000001) == 1;
        assert!(current_indicator);
        let _section_number = table_syntax_section[3];
        let _last_section_number = table_syntax_section[4];

        let table_data = &table_syntax_section[5 .. section_length - 4];

        let update_programs = self.table_processor.process(table_data);

        // poly: 0x04C11DB7, init: 0xFFFFFFFF, no ref-in/ref-out/xor-out 
        let _crc32 = &table_syntax_section[section_length - 4 .. section_length];

//      println!("table_syntax_section: id={}, version_number={}, {}, section_number={}..{}",
//               _table_id_extension,
//               _syntax_version_number,
//               if current_indicator { "current" } else { "next" },
//               _section_number,
//               _last_section_number);

        Ok(update_programs)
    }
}

fn main() -> io::Result<()>  {
    let n = match std::env::args().nth(1)
    { Some(n) => n.parse::<usize>().unwrap(), None => 1 };
    let reader = File::open("elephants.ts")?;
    let mut reader = BufReader::with_capacity(n*PACKET_SIZE, reader);
    let mut programs = ProgramMap::new();
    let pat = ProgramAssociationTable {}; 
    programs.insert(0, Box::new(ProgramSpecificInformation { table_processor: pat}) as Box<dyn PacketProcessor>);

    let mut packet = [0; PACKET_SIZE];
    let mut count = 0;
    while PACKET_SIZE == reader.read(&mut packet)? {
        assert_eq!(packet[0], 0x47);
        let pid = get_pid(&packet);
        match programs.get_mut(&pid) {
          Some(program) => {
            let update_programs = program.process(&packet)?;
            update_programs(&mut programs);
          },
          _ => { panic!("Unknown PID: {}", pid); }
        }

        count += 1;
    }
    println!("Read: {} packets", count);
    Ok(())
}